//! Graphical ITS-Board emulator.

mod app;
mod board;
mod emu;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about = "ITS-Board emulator (graphical)")]
struct Args {
    /// Firmware image (.axf / .elf) to start with
    firmware: Option<PathBuf>,
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("ITS-Board Emulator")
            .with_inner_size([1280.0, 1000.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("ITS-Board Emulator", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, args.firmware)))))
}
