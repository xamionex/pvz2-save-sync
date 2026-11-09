mod adb;
mod app;
mod config;
mod game;
mod pairing;
mod qr;
mod save;

use crate::app::SaveSyncApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("PVZ2 Save Sync v{}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([980.0, 620.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PVZ2 Save Sync",
        options,
        Box::new(|cc| Ok(Box::new(SaveSyncApp::new(cc)))),
    )
}
