# Hayate (疾風)

A fast, keyboard-driven file manager for Windows, written in Rust, with first-class Japanese support.

## Features

- Instant folder listing on a background thread; virtualised list that only draws visible rows, so folders with 100k+ files stay smooth.
- Single or dual pane (`Ctrl+\`), tab to switch.
- Filter as you type (`Ctrl+F`) and recursive search (`Ctrl+Shift+F`) that streams results while it scans.
- Japanese-aware matching and sorting: full-width/half-width, hiragana/katakana and case are treated as equal, so `てすと` finds `テスト.txt` and `ﾃｽﾄ.txt`. Numbers sort naturally (`file2` before `file10`, `画像２` before `画像10`).
- Japanese UI fonts loaded from the system (Yu Gothic UI → Meiryo UI → MS Gothic). Override with the `HAYATE_FONT` environment variable.
- IME input (日本語入力) works in every text field: rename, filter, search, path bar.
- Copy / cut / paste, rename in place (F2), new folder, Recycle Bin delete, permanent delete with confirmation, drag files in from Explorer.
- Go-to palette (`Ctrl+P`) over quick-access, recent folders and drives.
- Press `F1` for every shortcut.

## Why egui / eframe

| | egui (chosen) | Slint | iced | Win32/WinUI |
|---|---|---|---|---|
| Startup and redraw speed | Very fast, GPU, immediate mode | Fast | Fast | Fastest |
| Japanese IME | Works via winit | Works | Partial | Native |
| CJK fonts | Load any system font | Yes | Yes | Native |
| Single small .exe, no runtime | Yes | Yes | Yes | WinUI needs runtime |
| Cross-compile from Linux | Yes (mingw) | Yes | Yes | Harder |
| Effort for a dense custom list view | Low | Medium | Medium | High |

egui gives the best mix of raw speed, a fully custom look, and easy iteration. The file list is hand-painted rather than built from widgets, which is what keeps it fast.

## Build

Windows (native):

```
cargo build --release
target\release\hayate.exe
```

From Linux (cross-compile):

```
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

GitHub Actions builds `hayate.exe` on every push (`.github/workflows/windows.yml`) and attaches it to releases for `v*` tags.

## Keyboard

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn Home End | Move (Shift extends selection) |
| Enter | Open |
| Backspace, Alt+↑ | Parent folder |
| Alt+← / Alt+→ | Back / forward |
| Space / Ctrl+A | Toggle / select all |
| Ctrl+F / Ctrl+Shift+F | Filter / search subfolders |
| Ctrl+P / Ctrl+L | Go to folder / edit path |
| Ctrl+C X V | Copy, cut, paste |
| Ctrl+Shift+C | Copy full path |
| F2 | Rename |
| Delete / Shift+Delete | Recycle Bin / delete permanently |
| Ctrl+Shift+N | New folder |
| Ctrl+\\ / Tab | Dual pane / switch pane |
| Ctrl+H | Show hidden files |
| F5 | Refresh |
