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

    /// Address of the GDB server (`target remote` in GDB / port in the VS Code
    /// debug configuration). Default 127.0.0.1:3333; with --usb only if given
    #[arg(long, value_name = "ADDR")]
    gdb: Option<String>,

    /// Do not start the GDB server
    #[arg(long, conflicts_with = "gdb")]
    no_gdb: bool,

    /// Export the board's ST-LINK (debug probe and virtual COM port) over
    /// USB/IP (default address 127.0.0.1:3240). On Linux,
    /// `sudo usbip attach -r 127.0.0.1 -b 1-1` plugs it in (vhci-hcd module);
    /// then pyOCD (the course's STLink@pyOCD configurations), OpenOCD and
    /// terminal programs use it like the real board's
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:3240")]
    usb: Option<String>,

    /// With --usb: run `sudo usbip attach` for it
    #[arg(long, requires = "usb")]
    usb_attach: bool,

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
        // dithering adds noise that makes the small print of the board drawing hard to read
        dithering: false,
        ..Default::default()
    };
    eframe::run_native(
        "ITS-Board Emulator",
        options,
        Box::new(move |cc| {
            // pyOCD's GDB server takes port 3333 when the ST-LINK is used.
            let gdb = match args.gdb {
                Some(addr) => Some(addr),
                None if args.no_gdb || args.usb.is_some() => None,
                None => Some("127.0.0.1:3333".to_owned()),
            };
            let usb = args.usb.map(|addr| emu::UsbConfig { addr, attach: args.usb_attach });
            Ok(Box::new(app::App::new(cc, args.firmware, gdb, usb, args.ideal_colors)))
        }),
    )
}
