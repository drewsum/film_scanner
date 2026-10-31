//! Film Scanner: an open-source film scanning workflow (scan, crop, invert, adjust, export).

mod state;
mod ui;
mod view;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Film Scanner")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Film Scanner",
        options,
        Box::new(|cc| Ok(Box::new(ui::ScannerApp::new(cc)))),
    )
}
