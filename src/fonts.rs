//! Font setup.
//!
//! The UI uses "8823 Gothic", bundled into the binary: JetBrains Mono for Latin and
//! BIZ UDゴシック for Japanese, merged into ONE font by `tools/build_font.py`.
//! Because both scripts share one set of vertical metrics, mixed names like
//! "2610_研究AIを作る.pdf" sit on a single baseline. (Pairing a Latin font with a
//! separate Japanese fallback makes kana/kanji ride higher or lower than the Latin.)
//! The system's Japanese font stays loaded as a last resort for rare glyphs.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::path::PathBuf;
use std::sync::Arc;

/// (file, face index inside a .ttc)
fn candidates() -> Vec<(PathBuf, u32)> {
    let mut v = Vec::new();
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
    #[cfg(target_os = "macos")]
    {
        // Hiragino Sans (ヒラギノ角ゴシック) ships with every macOS; W4 reads best at UI sizes.
        for w in ["W4", "W3", "W5"] {
            v.push((PathBuf::from(format!("/System/Library/Fonts/ヒラギノ角ゴシック {w}.ttc")), 0));
        }
        v.push((PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"), 0));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        for p in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        ] {
            v.push((PathBuf::from(p), 0));
        }
    }
    v
}

static BUNDLED: &[u8] = include_bytes!("../assets/fonts/8823Gothic-Regular.ttf");

/// Returns a description of the font in use, for the status bar.
pub fn install(ctx: &egui::Context) -> Option<String> {
    let mut fonts = FontDefinitions::default();
    let mut primary = vec!["8823-gothic".to_string()];
    fonts.font_data.insert("8823-gothic".into(), Arc::new(FontData::from_static(BUNDLED)));
    let mut desc = "8823 Gothic (JetBrains Mono + BIZ UDゴシック)".to_string();

    // FONT_8823=/path/to/font.ttf overrides the UI font.
    if let Some(p) = std::env::var_os("FONT_8823") {
        if let Ok(bytes) = std::fs::read(&p) {
            fonts.font_data.insert("user".into(), Arc::new(FontData::from_owned(bytes)));
            primary.insert(0, "user".into());
            desc = PathBuf::from(p).display().to_string();
        }
    }
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&fam) {
            for (i, name) in primary.iter().enumerate() {
                list.insert(i, name.clone());
            }
        }
    }

    // System Japanese font as a last resort for anything the bundled font lacks.
    for (path, index) in candidates() {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let index = if index < ttc_face_count(&bytes) { index } else { 0 };
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        fonts.font_data.insert("ja-system".into(), Arc::new(data));
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&fam) {
                list.push("ja-system".into());
            }
        }
        break;
    }

    ctx.set_fonts(fonts);
    Some(desc)
}

/// Number of faces in a TrueType collection (1 for a plain .ttf/.otf).
fn ttc_face_count(b: &[u8]) -> u32 {
    if b.len() >= 12 && &b[0..4] == b"ttcf" {
        u32::from_be_bytes([b[8], b[9], b[10], b[11]])
    } else {
        1
    }
}
