//! Font setup. egui's built-in fonts have no CJK glyphs, so we load the system's
//! Japanese UI fonts at startup and register them as fallbacks for every family.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::path::PathBuf;
use std::sync::Arc;

/// (file, face index inside a .ttc)
fn candidates() -> Vec<(PathBuf, u32)> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("FONT_8823") {
        v.push((PathBuf::from(p), 0));
    }
    #[cfg(windows)]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
            .join("Fonts");
        // Yu Gothic UI (Win 8.1+), Meiryo UI, MS Gothic as last resort.
        v.push((fonts.join("YuGothM.ttc"), 1));
        v.push((fonts.join("YuGothR.ttc"), 1));
        v.push((fonts.join("meiryo.ttc"), 2));
        v.push((fonts.join("msgothic.ttc"), 0));
    }
    #[cfg(not(windows))]
    {
        for p in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        ] {
            v.push((PathBuf::from(p), 0));
        }
    }
    v
}

#[cfg(windows)]
fn latin_ui_font() -> Option<Vec<u8>> {
    let dir = std::env::var_os("WINDIR").map(PathBuf::from)?.join("Fonts");
    std::fs::read(dir.join("segoeui.ttf")).ok()
}

#[cfg(not(windows))]
fn latin_ui_font() -> Option<Vec<u8>> {
    None
}

/// Returns the name of the Japanese font that was loaded, if any.
pub fn install(ctx: &egui::Context) -> Option<String> {
    let mut fonts = FontDefinitions::default();
    let mut loaded = None;

    if let Some(bytes) = latin_ui_font() {
        fonts.font_data.insert("ui-latin".into(), Arc::new(FontData::from_owned(bytes)));
        if let Some(f) = fonts.families.get_mut(&FontFamily::Proportional) {
            f.insert(0, "ui-latin".into());
        }
    }

    for (path, index) in candidates() {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let index = if index < ttc_face_count(&bytes) { index } else { 0 };
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        fonts.font_data.insert("ja".into(), Arc::new(data));
        // Appended as a fallback: Latin and the bundled icon/emoji fonts keep priority,
        // every kana/kanji glyph they lack comes from here.
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&fam) {
                list.push("ja".into());
            }
        }
        loaded = Some(path.display().to_string());
        break;
    }

    ctx.set_fonts(fonts);
    loaded
}

/// Number of faces in a TrueType collection (1 for a plain .ttf/.otf).
fn ttc_face_count(b: &[u8]) -> u32 {
    if b.len() >= 12 && &b[0..4] == b"ttcf" {
        u32::from_be_bytes([b[8], b[9], b[10], b[11]])
    } else {
        1
    }
}
