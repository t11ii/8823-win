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

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("8823")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([520.0, 320.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("8823", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
