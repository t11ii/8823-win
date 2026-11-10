//! Small per-OS differences: wording, shortcut labels, and handing off to the OS shell.

use std::path::Path;

#[cfg(target_os = "macos")]
pub const TRASH: &str = "Trash";
#[cfg(not(target_os = "macos"))]
pub const TRASH: &str = "Recycle Bin";

#[cfg(target_os = "macos")]
pub const FILE_BROWSER: &str = "Finder";
#[cfg(windows)]
pub const FILE_BROWSER: &str = "Explorer";
#[cfg(not(any(windows, target_os = "macos")))]
pub const FILE_BROWSER: &str = "file manager";

#[cfg(target_os = "macos")]
pub const BACK_KEY: &str = "Ctrl+[";
#[cfg(not(target_os = "macos"))]
pub const BACK_KEY: &str = "Alt+←";

#[cfg(target_os = "macos")]
pub const FWD_KEY: &str = "Ctrl+]";
#[cfg(not(target_os = "macos"))]
pub const FWD_KEY: &str = "Alt+→";

#[cfg(target_os = "macos")]
pub const DELETE_KEY: &str = "Ctrl+⌫";
#[cfg(not(target_os = "macos"))]
pub const DELETE_KEY: &str = "Del";

/// Shortcut labels are written Windows-style ("Ctrl+Shift+C"); on macOS they
/// become the usual symbols ("⇧⌘C").
pub fn keys(s: &str) -> String {
    if !cfg!(target_os = "macos") {
        return s.to_string();
    }
    s.split(" / ")
        .map(|combo| {
            let mut mods = String::new();
            let mut rest = combo;
            loop {
                if let Some(r) = rest.strip_prefix("Ctrl+") {
                    mods.push('⌘');
                    rest = r;
                } else if let Some(r) = rest.strip_prefix("Shift+") {
                    mods.insert(0, '⇧');
                    rest = r;
                } else if let Some(r) = rest.strip_prefix("Alt+") {
                    mods.insert(0, '⌥');
                    rest = r;
                } else {
                    break;
                }
            }
            // Apple orders modifiers ⌃⌥⇧⌘.
            let mut ordered: Vec<char> = mods.chars().collect();
            ordered.sort_by_key(|c| "⌃⌥⇧⌘".find(*c).unwrap_or(9));
            let rest = match rest {
                "Backspace" => "⌫",
                "Delete" | "Del" => "⌦",
                "Enter" => "↩",
                "Esc" => "⎋",
                "Tab" => "⇥",
                r => r,
            };
            format!("{}{}", ordered.into_iter().collect::<String>(), rest)
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

pub fn help() -> Vec<(String, String)> {
    let mut v: Vec<(&str, &str)> = vec![
        ("↑ ↓ PgUp PgDn Home End", "Move (Shift extends selection)"),
        ("Enter / double-click", "Open folder or file"),
    ];
    if cfg!(target_os = "macos") {
        v.extend([
            ("Ctrl+O / Ctrl+↓", "Open"),
            ("Ctrl+↑ / Backspace", "Parent folder"),
            ("Ctrl+[ / Ctrl+]", "Back / forward"),
        ]);
    } else {
        v.extend([("Backspace / Alt+↑", "Parent folder"), ("Alt+← / Alt+→", "Back / forward")]);
    }
    v.extend([
        ("Space", "Toggle selection"),
        ("Ctrl+A", "Select all"),
        ("Ctrl+F", "Filter this folder"),
        ("Ctrl+Shift+F", "Search subfolders"),
        ("Ctrl+P", "Go to folder"),
        ("Ctrl+L", "Edit path"),
        ("Ctrl+C / Ctrl+X / Ctrl+V", "Copy / cut / paste"),
        ("F2", "Rename"),
        ("Ctrl+Shift+N", "New folder"),
        ("Ctrl+T / Ctrl+W", "New tab / close tab"),
        ("Ctrl+Tab / Ctrl+1…9", "Next tab / go to tab (drag a tab to reorder)"),
        ("Ctrl+Enter, middle-click", "Open folder in new tab"),
        ("(just type)", "Filter this folder; ↑↓ to pick, Enter to open"),
        ("Ctrl+G", "List / gallery view"),
        ("Ctrl+\\", "Toggle dual pane"),
        ("Tab", "Switch pane"),
        ("F5", "Refresh"),
        ("Esc", "Clear filter / close search"),
    ]);
    if cfg!(target_os = "macos") {
        v.extend([
            ("Ctrl+Shift+G", "Go to folder"),
            ("Ctrl+Alt+C / Ctrl+Shift+C", "Copy full path"),
            ("Ctrl+Backspace", "Move to Trash"),
            ("Ctrl+Alt+Backspace", "Delete permanently"),
            ("Ctrl+Shift+.", "Show hidden files"),
        ]);
    } else {
        v.extend([
            ("Ctrl+Shift+C", "Copy full path"),
            ("Delete / Shift+Delete", "Recycle Bin / delete permanently"),
            ("Ctrl+H", "Show hidden files"),
        ]);
    }
    v.into_iter().map(|(a, b)| (keys(a), b.to_string())).collect()
}

/// Show the item selected in Explorer / Finder.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if let Some(parent) = path.parent() {
            let _ = open::that_detached(parent);
        }
    }
}

pub fn terminal(dir: &Path) {
    #[cfg(windows)]
    {
        // Prefer Windows Terminal, fall back to cmd.
        let ok = std::process::Command::new("wt.exe").arg("-d").arg(dir).spawn().is_ok();
        if !ok {
            let _ = std::process::Command::new("cmd").args(["/c", "start", "", "cmd"]).current_dir(dir).spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").args(["-a", "Terminal"]).arg(dir).spawn();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = std::process::Command::new("x-terminal-emulator").current_dir(dir).spawn();
    }
}
