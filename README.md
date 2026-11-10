# 8823 (はやぶさ)

A fast, keyboard-driven file manager for Windows and macOS, written in Rust, with first-class Japanese support.

## Features

- Instant folder listing on a background thread; virtualised list that only draws visible rows, so folders with 100k+ files stay smooth.
- Single or dual pane (`Ctrl+\`), tab to switch.
- Filter as you type (`Ctrl+F`) and recursive search (`Ctrl+Shift+F`) that streams results while it scans.
- Japanese-aware matching and sorting: full-width/half-width, hiragana/katakana and case are treated as equal, so `てすと` finds `テスト.txt` and `ﾃｽﾄ.txt`. Numbers sort naturally (`file2` before `file10`, `画像２` before `画像10`).
- Bundled UI font **8823 Gothic**: JetBrains Mono (Latin) and BIZ UDゴシック (Japanese) merged into one font, so mixed Japanese/English names share one baseline. Built by `tools/build_font.py`; both are SIL OFL 1.1 (`assets/fonts/OFL.txt`). The system Japanese font (Yu Gothic UI / Hiragino) stays as a fallback; override with the `FONT_8823` environment variable.
- macOS file names stored in decomposed form (NFD, e.g. ガ as カ + ゛) are shown composed, and match searches typed in normal (NFC) form.
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
target\release\8823.exe
```

From Linux (cross-compile):

```
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

macOS (universal .app for Apple Silicon and Intel):

```
./scripts/bundle-mac.sh      # → dist/8823-mac.zip
```

The app is ad-hoc signed, not notarized. On first launch, right-click 8823.app → Open (or run `xattr -dr com.apple.quarantine 8823.app`).

GitHub Actions (`.github/workflows/build.yml`) builds `8823.exe` and `8823-mac.zip` on every push and attaches both to releases for `v*` tags.

## macOS differences

- Shortcuts use ⌘ wherever Windows uses Ctrl, plus Finder conventions: ⌘⌫ Trash, ⌥⌘⌫ delete permanently, ⌘↑ parent, ⌘↓ / ⌘O open, ⌘[ ⌘] back/forward, ⇧⌘G go to folder, ⇧⌘. hidden files, ⌥⌘C copy path.
- Sidebar lists `/Volumes` (external disks, DMGs, network shares), iCloud Drive and `~/Library/CloudStorage` (Dropbox, Google Drive, OneDrive…).
- Files flagged hidden with `chflags hidden` are treated as hidden, like dotfiles.

## Keyboard (Windows; press F1 in the app for the current platform)

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
