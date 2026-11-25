//! XCL Launcher – a small game library and launcher for XCL, the Xbox Series X|S compatibility
//! layer.

// no console window for the launcher itself (release builds; debug builds keep it for output)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod clean;
mod config;
mod game;
mod paths;
mod process;
mod settings_ui;
mod thumbs;
mod xcl_toml;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("XCL Launcher")
            .with_app_id("xcl-launcher")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([560.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "XCL Launcher",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
