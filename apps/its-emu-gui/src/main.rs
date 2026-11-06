//! Graphical ITS-Board emulator.

// No console window next to the GUI on Windows (debug builds keep it for the log)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod board;
mod devices;
mod emu;
mod files;
mod pins;
mod render;
mod settings;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(version, about = "ITS-Board emulator (graphical)")]
struct Args {
    /// Firmware image (.axf / .elf) to start with
    firmware: Option<PathBuf>,

    /// Address of the GDB server (`target remote` in GDB / port in the VS Code
    /// debug configuration). While pyOCD uses the ST-LINK the server leaves
    /// the port to pyOCD's own GDB server
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:3333")]
    gdb: String,

    /// Do not start the GDB server
    #[arg(long)]
    no_gdb: bool,

    /// Plug in the board's ST-LINK (debug probe and virtual COM port) over
    /// USB/IP at ADDR (default 127.0.0.1:3240), whatever the toolbar setting
    /// says. On Linux it is attached with `usbip attach` (vhci-hcd module);
    /// then pyOCD (the course's STLink@pyOCD configurations), OpenOCD and
    /// terminal programs use it like the real board's
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:3240")]
    usb: Option<String>,

    /// Do not plug in the ST-LINK this time (see --usb)
    #[arg(long, conflicts_with = "usb")]
    no_usb: bool,

    /// Only export the ST-LINK; attach it yourself (`sudo usbip attach -r 127.0.0.1 -b 1-1`)
    #[arg(long)]
    no_usb_attach: bool,

    /// External device on header pins (repeatable), instead of the ones
    /// added in the Devices panel last time: `encoder@PF0,PF1`,
    /// `ds18b20@PG2`, `pico=FIRMWARE.uf2@GP0=PF0,GP5=PF1` (a Raspberry Pi Pico
    /// running the firmware, its GPIOs wired to the pins)
    #[arg(long = "device", value_name = "DEVICE@PINS", value_parser = itsboard::spec::DeviceSpec::parse)]
    devices: Vec<itsboard::spec::DeviceSpec>,

    /// Boot ROM of the Pico's RP2040 (16 KiB, or the ELF file of the
    /// pico-bootrom release), instead of the one chosen in the Devices panel.
    /// Default: downloaded once from the Raspberry Pi GitHub release and kept
    /// in the cache directory
    #[arg(long, value_name = "FILE")]
    pico_bootrom: Option<PathBuf>,

    /// Show the display with the frame buffer's ideal colours instead of the
    /// look of the real panel (darker mid tones, grey black)
    #[arg(long)]
    ideal_colors: bool,

    /// Graphics backend. auto: wgpu (DirectX 12, Metal), and OpenGL where
    /// wgpu finds no graphics adapter (virtual machines, remote desktops).
    /// Linux always uses OpenGL
    #[cfg(not(target_os = "linux"))]
    #[arg(long, value_enum, default_value_t = RendererChoice::Auto)]
    renderer: RendererChoice,
}

#[cfg(not(target_os = "linux"))]
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq)]
enum RendererChoice {
    Auto,
    Wgpu,
    Gl,
}

fn main() -> eframe::Result {
    #[cfg(all(windows, not(debug_assertions)))]
    attach_console();
    // Hidden: warnings about the desktop that do not affect the emulator, such
    // as X servers without an XSETTINGS daemon (winit; window managers without
    // a desktop), no accessibility bus (zbus) or the C locale (font provider);
    // eframe logs the wgpu error that main falls back from (and returns all others)
    let filter = "warn,winit=error,zbus=error,egui_system_font_provider=error,eframe::native::run=off";
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter)).init();
    let args = Args::parse();
    #[cfg(target_os = "linux")]
    return run(args, eframe::Renderer::Glow);
    #[cfg(not(target_os = "linux"))]
    {
        if args.renderer == RendererChoice::Gl {
            return run(args, eframe::Renderer::Glow);
        }
        match run(args.clone(), eframe::Renderer::Wgpu) {
            Err(eframe::Error::Wgpu(e)) if args.renderer == RendererChoice::Auto => {
                log::warn!("wgpu: {e}; using OpenGL");
                run(args, eframe::Renderer::Glow)
            }
            result => result,
        }
    }
}

fn run(args: Args, renderer: eframe::Renderer) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("ITS-Board Emulator")
            // matches the desktop file of the Linux package (window icon, taskbar grouping)
            .with_app_id("its-emu-gui")
            .with_inner_size([1280.0, 1000.0])
            .with_drag_and_drop(true),
        renderer,
        // dithering adds noise that makes the small print of the board drawing hard to read
        dithering: false,
        ..Default::default()
    };
    eframe::run_native(
        "ITS-Board Emulator",
        options,
        Box::new(move |cc| {
            let gdb = (!args.no_gdb).then_some(args.gdb);
            let mut settings = settings::Settings::load();
            if !args.devices.is_empty() {
                settings.devices = args.devices;
            }
            let usb = emu::UsbConfig {
                enabled: if args.no_usb { false } else { args.usb.is_some() || settings.usb },
                addr: args.usb.unwrap_or_else(|| format!("127.0.0.1:{}", stlink::DEFAULT_PORT)),
                attach: !args.no_usb_attach,
            };
            let bootrom = args.pico_bootrom.or_else(|| settings.pico_bootrom.clone());
            Ok(Box::new(app::App::new(cc, args.firmware, gdb, usb, bootrom, settings, args.ideal_colors)))
        }),
    )
}

/// Started from a terminal, the GUI (without a console of its own) writes
/// its help and log there.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no arguments besides the constant; fails harmlessly without a parent console
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}
