//! Small per-user settings, kept as plain text so they are easy to read, edit and back up.
//!
//! Folder: `%APPDATA%\8823` on Windows, `~/Library/Application Support/8823` on macOS,
//! `$XDG_CONFIG_HOME/8823` (or `~/.config/8823`) elsewhere. `CONFIG_8823` overrides it.

use std::path::PathBuf;

const FAVORITES: &str = "favorites.txt";

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CONFIG_8823").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    {
        if let Some(a) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
            return PathBuf::from(a).join("8823");
        }
    }
    #[cfg(target_os = "macos")]
    {
        return crate::fsx::home_dir().join("Library/Application Support/8823");
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
            return PathBuf::from(x).join("8823");
        }
        crate::fsx::home_dir().join(".config").join("8823")
    }
}

/// The saved favourite folders in sidebar order, or `None` if the user has never changed them.
pub fn load_favorites() -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(dir().join(FAVORITES)).ok()?;
    Some(parse_favorites(&text))
}

/// One folder per line; blank lines and `#` comments are skipped, repeats dropped.
fn parse_favorites(text: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let p = PathBuf::from(line);
        if !v.contains(&p) {
            v.push(p);
        }
    }
    v
}

fn format_favorites(paths: &[PathBuf]) -> String {
    let mut s = String::from("# 8823 favorites: one folder per line, in sidebar order.\n");
    for p in paths {
        s.push_str(&p.to_string_lossy());
        s.push('\n');
    }
    s
}

/// Written to a temporary file first, so a crash mid-write cannot leave a half list behind.
pub fn save_favorites(paths: &[PathBuf]) -> std::io::Result<()> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let tmp = d.join(format!("{FAVORITES}.tmp"));
    std::fs::write(&tmp, format_favorites(paths))?;
    std::fs::rename(&tmp, d.join(FAVORITES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_roundtrip_with_japanese_and_spaces() {
        let paths = vec![PathBuf::from("/home/太郎/研究 メモ"), PathBuf::from("C:\\Users\\taro\\ドキュメント"), PathBuf::from("/Volumes/外付け")];
        assert_eq!(parse_favorites(&format_favorites(&paths)), paths);
    }

    #[test]
    fn favorites_skip_comments_blanks_repeats_and_bom() {
        let text = "\u{feff}# comment\r\n/a/b\r\n\r\n/a/b\n/c d\n";
        assert_eq!(parse_favorites(text), [PathBuf::from("/a/b"), PathBuf::from("/c d")]);
        assert!(parse_favorites("").is_empty());
    }
}
