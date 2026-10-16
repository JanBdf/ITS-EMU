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

    /// Address of the GDB server (`target remote` in GDB / port in the VS Code debug configuration)
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:3333")]
    gdb: String,

    /// Do not start the GDB server
    #[arg(long)]
    no_gdb: bool,

    /// Show the display with the frame buffer's ideal colours instead of the
    /// look of the real panel (darker mid tones, grey black)
    #[arg(long)]
    ideal_colors: bool,
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
    eframe::run_native(
        "ITS-Board Emulator",
        options,
        Box::new(|cc| {
            Ok(Box::new(app::App::new(cc, args.firmware, (!args.no_gdb).then_some(args.gdb), args.ideal_colors)))
        }),
    )
}
