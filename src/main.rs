#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod config;
mod fonts;
mod fsx;
mod pane;
mod plat;
mod text;
mod theme;
mod thumbs;

/// Window, taskbar and Dock icon. Without it eframe shows its own default logo
/// (on the Mac it would also replace the .app bundle's icon while running).
fn app_icon() -> Option<std::sync::Arc<eframe::egui::IconData>> {
    #[cfg(target_os = "macos")]
    let png: &[u8] = include_bytes!("../assets/icon/icon-512.png");
    #[cfg(not(target_os = "macos"))]
    let png: &[u8] = include_bytes!("../assets/icon/icon-256.png");
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(std::sync::Arc::new(eframe::egui::IconData { rgba: img.into_raw(), width, height }))
}

fn main() -> eframe::Result {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("8823")
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([520.0, 320.0])
        .with_drag_and_drop(true);
    viewport.icon = app_icon();
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("8823", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_icon_decodes() {
        let icon = super::app_icon().expect("icon decodes");
        assert!(icon.width >= 256 && icon.width == icon.height);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }
}
