//! Film Scanner: an open-source film scanning workflow (scan, crop, invert, adjust, export).

// Release builds on Windows are a GUI program: no console window opens with them.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod state;
mod ui;
mod view;
mod worker;

use eframe::egui;

/// Skips the elevation at launch; native Epson scans then fail with a message.
const NO_ELEVATE: &str = "--no-elevate";

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if elevate() {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Film Scanner")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("the bundled icon is a PNG"),
            ),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Film Scanner",
        options,
        Box::new(|cc| Ok(Box::new(ui::ScannerApp::new(cc)))),
    );
    // A scan still running when the window closed may have the WIA service stopped.
    scan_epson::system::restore_wia();
    result
}

/// Native scans of an attached V500 need administrator rights (to stop the WIA service), so
/// ask for them once, at launch, by starting an elevated copy. Returns whether one started
/// and this process should exit.
fn elevate() -> bool {
    use scan_core::ScannerBackend;
    if std::env::args().any(|a| a == NO_ELEVATE) || scan_epson::system::is_elevated() {
        return false;
    }
    let has_v500 = scan_epson::EpsonBackend
        .list_devices()
        .is_ok_and(|d| !d.is_empty());
    if !has_v500 {
        return false;
    }
    match scan_epson::system::relaunch_elevated() {
        Ok(()) => true,
        Err(e) => {
            log::warn!("continuing without administrator rights: {e}");
            false
        }
    }
}
