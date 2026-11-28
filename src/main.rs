#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod config;
mod dragout;
#[cfg(windows)]
mod folder_app;
mod fonts;
mod fsx;
mod osimg;
mod pace;
mod pane;
mod plat;
mod preview;
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

fn app_viewport() -> eframe::egui::ViewportBuilder {
    let version = concat!("8823 v", env!("CARGO_PKG_VERSION"));
    // The rolling GitHub release uses the same Cargo version across commits.
    let title = match option_env!("GITHUB_SHA").and_then(|sha| sha.get(..7)) {
        Some(commit) => format!("{version} ({commit})"),
        None => version.to_owned(),
    };
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([520.0, 320.0])
        .with_drag_and_drop(true);
    viewport.icon = app_icon();
    viewport
}

fn main() -> eframe::Result {
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: app_viewport(),
        ..Default::default()
    };
    // Windows: don't block in SwapBuffers until vsync. With vsync every frame stalled the UI
    // thread for one refresh period (31 ms on a 32 Hz Remote Desktop display), so scrolling
    // and key repeat crawled. Windowed GL output is composited by DWM, so this does not tear.
    // The frame rate is capped in `pace.rs` instead.
    #[cfg(windows)]
    {
        options.glow_options.vsync = false;
    }
    eframe::run_native("8823", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_title_identifies_the_build() {
        let title = super::app_viewport().title.expect("window title");
        assert!(title.contains(concat!("v", env!("CARGO_PKG_VERSION"))), "version missing from {title:?}");
        if let Some(commit) = option_env!("GITHUB_SHA").and_then(|sha| sha.get(..7)) {
            assert!(title.ends_with(&format!("({commit})")), "commit missing from {title:?}");
        }
    }

    #[test]
    fn app_icon_decodes() {
        let icon = super::app_icon().expect("icon decodes");
        assert!(icon.width >= 256 && icon.width == icon.height);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }
}
