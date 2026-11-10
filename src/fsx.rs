//! File-system model: directory listing, sorting, recursive search, file operations.
//! Everything slow runs on background threads and reports back over a channel.

use crate::text::{fold, natural_cmp};
use std::cmp::Ordering;
use std::ffi::OsString;
use std::fs::{FileType, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AO};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hidden: bool,
    /// Folded name used for sorting and filtering.
    pub key: String,
    /// Folded extension.
    pub ext: String,
    /// For search results: folder relative to the search root.
    pub rel: Option<String>,
}

impl Entry {
    pub fn new(path: PathBuf, name: OsString, md: Option<Metadata>, ft: Option<FileType>) -> Self {
        let name = name.to_string_lossy().into_owned();
        let is_link = ft.map(|t| t.is_symlink()).unwrap_or(false);
        let mut is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        if is_link {
            // Junctions and directory symlinks: follow once to see what they point at.
            is_dir = std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        }
        let (size, modified) = match &md {
            Some(m) => (if is_dir { 0 } else { m.len() }, m.modified().ok()),
            None => (0, None),
        };
        let hidden = is_hidden(&name, md.as_ref());
        let key = fold(&name);
        let ext = if is_dir {
            String::new()
        } else {
            key.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default()
        };
        Entry { name, path, is_dir, is_link, size, modified, hidden, key, ext, rel: None }
    }
}

#[cfg(windows)]
fn is_hidden(_name: &str, md: Option<&Metadata>) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    md.map(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0).unwrap_or(false)
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _md: Option<&Metadata>) -> bool {
    name.starts_with('.')
}

pub fn list_dir(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(256);
    for de in rd.flatten() {
        // On Windows both of these come straight from FindNextFileW: no extra syscalls.
        let md = de.metadata().ok();
        let ft = de.file_type().ok();
        out.push(Entry::new(de.path(), de.file_name(), md, ft));
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Ext,
}

pub fn compare(a: &Entry, b: &Entry, key: SortKey, asc: bool) -> Ordering {
    // Folders always first, regardless of direction.
    match (a.is_dir, b.is_dir) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let by_name = || natural_cmp(&a.key, &b.key);
    let o = match key {
        SortKey::Name => by_name(),
        SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
        SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
        SortKey::Ext => a.ext.cmp(&b.ext).then_with(by_name),
    };
    if asc { o } else { o.reverse() }
}

// ---------------------------------------------------------------------------
// Background messages

pub enum Msg {
    Listing { pane: usize, req: u64, path: PathBuf, result: Result<Vec<Entry>, String> },
    SearchBatch { pane: usize, req: u64, items: Vec<Entry>, scanned: usize },
    SearchDone { pane: usize, req: u64, scanned: usize },
    OpProgress(String),
    OpDone { summary: String, errors: Vec<String> },
}

pub struct Bg {
    pub tx: Sender<Msg>,
    pub ctx: eframe::egui::Context,
}

impl Bg {
    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }
}

pub fn spawn_listing(bg: Bg, pane: usize, req: u64, path: PathBuf) {
    std::thread::spawn(move || {
        let result = list_dir(&path);
        bg.send(Msg::Listing { pane, req, path, result });
    });
}

/// Recursive search below `root` for names containing `query` (Japanese-folded).
/// Streams results in batches so the first hits appear immediately.
pub fn spawn_search(bg: Bg, pane: usize, req: u64, root: PathBuf, query: String, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let q = fold(&query);
        let terms: Vec<&str> = q.split_whitespace().collect();
        let mut stack = vec![root.clone()];
        let mut batch = Vec::new();
        let mut scanned = 0usize;
        let mut last = Instant::now();
        while let Some(dir) = stack.pop() {
            if cancel.load(AO::Relaxed) {
                return;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for de in rd.flatten() {
                scanned += 1;
                let ft = de.file_type().ok();
                let is_real_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
                let name = de.file_name();
                let key = fold(&name.to_string_lossy());
                if is_real_dir {
                    stack.push(de.path());
                }
                if !terms.is_empty() && terms.iter().all(|t| key.contains(t)) {
                    let mut e = Entry::new(de.path(), name, de.metadata().ok(), ft);
                    let rel = dir.strip_prefix(&root).unwrap_or(&dir).to_string_lossy().into_owned();
                    e.rel = Some(if rel.is_empty() { ".".into() } else { rel });
                    batch.push(e);
                }
            }
            if !batch.is_empty() && (batch.len() >= 500 || last.elapsed() > Duration::from_millis(80)) {
                bg.send(Msg::SearchBatch { pane, req, items: std::mem::take(&mut batch), scanned });
                last = Instant::now();
            }
        }
        if !batch.is_empty() {
            bg.send(Msg::SearchBatch { pane, req, items: batch, scanned });
        }
        bg.send(Msg::SearchDone { pane, req, scanned });
    });
}

// ---------------------------------------------------------------------------
// File operations

/// Pick a free name in `dir` like "name (2).ext" when `name` is taken.
pub fn unique_target(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for i in 2.. {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    unreachable!()
}

fn copy_recursive(src: &Path, dst: &Path, bg: &Bg, count: &mut usize) -> std::io::Result<()> {
    let md = std::fs::symlink_metadata(src)?;
    if md.is_dir() {
        std::fs::create_dir(dst)?;
        for de in std::fs::read_dir(src)? {
            let de = de?;
            copy_recursive(&de.path(), &dst.join(de.file_name()), bg, count)?;
        }
    } else {
        std::fs::copy(src, dst)?;
        *count += 1;
        if *count % 25 == 0 {
            bg.send(Msg::OpProgress(format!("Copied {count} files…")));
        }
    }
    Ok(())
}

pub fn spawn_paste(bg: Bg, sources: Vec<PathBuf>, dest: PathBuf, cut: bool) {
    std::thread::spawn(move || {
        let mut errors = Vec::new();
        let mut count = 0usize;
        let mut done = 0usize;
        for src in &sources {
            let Some(name) = src.file_name() else { continue };
            let name = name.to_string_lossy().into_owned();
            if dest.starts_with(src) {
                errors.push(format!("{name}: cannot paste a folder into itself"));
                continue;
            }
            // Moving within the same folder is a no-op.
            if cut && src.parent() == Some(dest.as_path()) {
                continue;
            }
            let target = unique_target(&dest, &name);
            let res = if cut {
                std::fs::rename(src, &target).or_else(|_| {
                    // Different volume: copy, then remove the original.
                    copy_recursive(src, &target, &bg, &mut count)?;
                    if src.is_dir() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) }
                })
            } else {
                copy_recursive(src, &target, &bg, &mut count)
            };
            match res {
                Ok(()) => done += 1,
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        let verb = if cut { "Moved" } else { "Copied" };
        bg.send(Msg::OpDone { summary: format!("{verb} {done} item(s)"), errors });
    });
}

pub fn spawn_delete(bg: Bg, paths: Vec<PathBuf>, permanent: bool) {
    std::thread::spawn(move || {
        let mut errors = Vec::new();
        let n = paths.len();
        if permanent {
            for p in &paths {
                let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
                if let Err(e) = r {
                    errors.push(format!("{}: {e}", p.display()));
                }
            }
        } else if let Err(e) = trash::delete_all(&paths) {
            errors.push(e.to_string());
        }
        let summary = if permanent {
            format!("Deleted {n} item(s) permanently")
        } else {
            format!("Moved {n} item(s) to the Recycle Bin")
        };
        bg.send(Msg::OpDone { summary, errors });
    });
}

// ---------------------------------------------------------------------------
// Places

pub fn drives() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        (0..26u8)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn quick_access() -> Vec<(String, PathBuf)> {
    let home = home_dir();
    let mut v = vec![("Home".to_string(), home.clone())];
    for (label, sub) in [
        ("Desktop", "Desktop"),
        ("Documents", "Documents"),
        ("Downloads", "Downloads"),
        ("Pictures", "Pictures"),
        ("Music", "Music"),
        ("Videos", "Videos"),
    ] {
        let p = home.join(sub);
        if p.is_dir() {
            v.push((label.to_string(), p));
        }
    }
    // OneDrive often redirects Desktop/Documents on Japanese Windows installs.
    if let Some(od) = std::env::var_os("OneDrive") {
        let od = PathBuf::from(od);
        if od.is_dir() {
            v.push(("OneDrive".to_string(), od));
        }
    }
    v
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < U.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{f:.1} {}", U[i]) }
}

pub fn format_time(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.format("%Y/%m/%d %H:%M").to_string()
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_names_roundtrip_through_fs() {
        let dir = std::env::temp_dir().join(format!("hayate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("サブフォルダ")).unwrap();
        std::fs::write(dir.join("日本語ファイル.txt"), "こんにちは").unwrap();
        std::fs::write(dir.join("ﾊﾝｶｸ.txt"), "").unwrap();
        let mut v = list_dir(&dir).unwrap();
        v.sort_by(|a, b| compare(a, b, SortKey::Name, true));
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names[0], "サブフォルダ");
        assert!(names.contains(&"日本語ファイル.txt"));
        assert!(names.contains(&"ﾊﾝｶｸ.txt"));
        assert_eq!(unique_target(&dir, "日本語ファイル.txt").file_name().unwrap(), "日本語ファイル (2).txt");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
