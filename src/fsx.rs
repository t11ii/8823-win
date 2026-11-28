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
        let name = crate::text::display_name(&name);
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

#[cfg(target_os = "macos")]
fn is_hidden(name: &str, md: Option<&Metadata>) -> bool {
    use std::os::macos::fs::MetadataExt;
    const UF_HIDDEN: u32 = 0x8000; // set by `chflags hidden`, used for ~/Library etc.
    name.starts_with('.') || md.map(|m| m.st_flags() & UF_HIDDEN != 0).unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn is_hidden(name: &str, _md: Option<&Metadata>) -> bool {
    name.starts_with('.')
}

pub fn list_dir(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(256);
    push_entries(rd, &mut out);
    #[cfg(windows)]
    merge_public_desktop(path, &mut out);
    Ok(out)
}

fn push_entries(rd: std::fs::ReadDir, out: &mut Vec<Entry>) {
    for de in rd.flatten() {
        // On Windows both of these come straight from FindNextFileW: no extra syscalls.
        let md = de.metadata().ok();
        let ft = de.file_type().ok();
        out.push(Entry::new(de.path(), de.file_name(), md, ft));
    }
}

/// Explorer shows the user's Desktop merged with the all-users Public Desktop (where
/// installers put their shortcuts). Do the same, so Desktop looks like the real desktop.
#[cfg(windows)]
fn merge_public_desktop(path: &Path, out: &mut Vec<Entry>) {
    use std::sync::OnceLock;
    static DIRS: OnceLock<Option<(PathBuf, PathBuf)>> = OnceLock::new();
    let dirs = DIRS.get_or_init(|| {
        let user = win::known_folder(&win::FOLDERID_Desktop)?;
        let public = win::known_folder(&win::FOLDERID_PublicDesktop)?;
        (user != public).then_some((user, public))
    });
    let Some((user, public)) = dirs else { return };
    if !same_dir(path, user) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(public) else { return };
    let mut extra = Vec::new();
    push_entries(rd, &mut extra);
    // A name on the user's own Desktop wins, as in Explorer.
    let mine: std::collections::HashSet<String> = out.iter().map(|e| e.key.clone()).collect();
    out.extend(extra.into_iter().filter(|e| !mine.contains(&e.key)));
}

/// Windows paths compare case-insensitively and may differ in a trailing separator.
#[cfg(windows)]
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    pub use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
        FOLDERID_PublicDesktop, FOLDERID_Videos,
    };
    use windows_sys::Win32::UI::Shell::{KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath};
    use windows_sys::core::GUID;

    /// Where Windows really keeps a standard folder. Desktop, Documents and Pictures are
    /// often redirected (OneDrive backup, a moved folder, a company policy), so
    /// %USERPROFILE%\Desktop can be an empty leftover while the real one is elsewhere.
    pub fn known_folder(id: &GUID) -> Option<PathBuf> {
        let mut p: *mut u16 = std::ptr::null_mut();
        // SAFETY: on success `p` is a NUL-terminated wide string we must free with
        // CoTaskMemFree; on failure it is NULL, and CoTaskMemFree(NULL) is a no-op.
        unsafe {
            let hr = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY as u32, std::ptr::null_mut(), &mut p);
            let out = if hr >= 0 && !p.is_null() {
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                Some(PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(p, len))))
            } else {
                None
            };
            windows_sys::Win32::System::Com::CoTaskMemFree(p as *const _);
            out.filter(|p| !p.as_os_str().is_empty())
        }
    }
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
    /// `sorted`: the order the entries are already in, so the UI thread need not sort them.
    Listing { pane: usize, req: u64, path: PathBuf, sorted: (SortKey, bool), result: Result<Vec<Entry>, String> },
    SearchBatch { pane: usize, req: u64, items: Vec<Entry>, scanned: usize },
    SearchDone { pane: usize, req: u64, scanned: usize },
    PreviewReady { req: u64, result: crate::preview::LoadResult },
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

/// List and sort `path` off the UI thread (sorting 100k names takes ~0.1 s).
pub fn spawn_listing(bg: Bg, pane: usize, req: u64, path: PathBuf, sort: SortKey, asc: bool) {
    std::thread::spawn(move || {
        let result = list_dir(&path).map(|mut v| {
            v.sort_by(|a, b| compare(a, b, sort, asc));
            v
        });
        bg.send(Msg::Listing { pane, req, path, sorted: (sort, asc), result });
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
            format!("Moved {n} item(s) to the {}", crate::plat::TRASH)
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
    #[cfg(target_os = "macos")]
    {
        // "/" plus every mounted volume (external disks, USB sticks, DMGs, network shares).
        let mut v = vec![PathBuf::from("/")];
        if let Ok(rd) = std::fs::read_dir("/Volumes") {
            let mut vols: Vec<PathBuf> = rd
                .flatten()
                .map(|d| d.path())
                // The boot volume shows up as a symlink to "/"; skip it.
                .filter(|p| !p.is_symlink())
                .collect();
            vols.sort();
            v.extend(vols);
        }
        v
    }
    #[cfg(not(any(windows, target_os = "macos")))]
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

/// The standard folders under the home folder, as (label, where it really is, the
/// plain `home/<label>` path that older versions used). The two paths differ when
/// Windows has redirected the folder, e.g. Desktop moved into OneDrive.
pub fn standard_folders() -> Vec<(&'static str, PathBuf, PathBuf)> {
    let home = home_dir();
    [
        "Desktop",
        "Documents",
        "Downloads",
        "Pictures",
        "Music",
        "Videos",
        "Movies",
        "Applications",
    ]
    .into_iter()
    .map(|label| {
        let legacy = home.join(label);
        (label, standard_folder(label).unwrap_or_else(|| legacy.clone()), legacy)
    })
    .collect()
}

#[cfg(windows)]
fn standard_folder(label: &str) -> Option<PathBuf> {
    let id = match label {
        "Desktop" => &win::FOLDERID_Desktop,
        "Documents" => &win::FOLDERID_Documents,
        "Downloads" => &win::FOLDERID_Downloads,
        "Pictures" => &win::FOLDERID_Pictures,
        "Music" => &win::FOLDERID_Music,
        "Videos" => &win::FOLDERID_Videos,
        _ => return None,
    };
    win::known_folder(id)
}

#[cfg(not(windows))]
fn standard_folder(_label: &str) -> Option<PathBuf> {
    None
}

pub fn quick_access(folders: &[(&str, PathBuf, PathBuf)]) -> Vec<(String, PathBuf)> {
    let home = home_dir();
    let mut v = vec![("Home".to_string(), home.clone())];
    for (label, p, _) in folders {
        if p.is_dir() {
            v.push((label.to_string(), p.clone()));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        if icloud.is_dir() {
            v.push(("iCloud Drive".to_string(), icloud));
        }
        if PathBuf::from("/Applications").is_dir() {
            v.push(("Applications".to_string(), PathBuf::from("/Applications")));
        }
        // Dropbox, OneDrive, Google Drive, Box… all live here on current macOS.
        if let Ok(rd) = std::fs::read_dir(home.join("Library/CloudStorage")) {
            for d in rd.flatten() {
                let name = crate::text::display_name(&d.file_name());
                v.push((name, d.path()));
            }
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
        let dir = std::env::temp_dir().join(format!("app8823-test-{}", std::process::id()));
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

    #[cfg(windows)]
    #[test]
    fn standard_folders_resolve_through_windows() {
        let desktop = win::known_folder(&win::FOLDERID_Desktop).expect("Desktop known folder");
        assert!(desktop.is_absolute(), "{desktop:?}");
        let (_, real, _) = standard_folders().into_iter().find(|(l, _, _)| *l == "Desktop").unwrap();
        assert_eq!(real, desktop);
        assert!(same_dir(Path::new(r"C:\Users\A\Desktop\"), Path::new(r"c:\users\a\desktop")));
        // Listing the Desktop must not fail even with the Public Desktop merged in.
        if desktop.is_dir() {
            list_dir(&desktop).unwrap();
        }
    }
}
