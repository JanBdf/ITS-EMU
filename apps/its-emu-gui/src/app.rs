//! egui front end: a picture of the ITS-Board with working LEDs, buttons and
//! display, plus a serial console.

use std::path::PathBuf;

use eframe::egui::{self, Color32, Key, RichText};
use itsboard::{SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::board::{self, BoardState};
use crate::emu::{self, Command, EmuHandle, RunState, Speed, UsbStatus};
use crate::settings::Settings;

const OK_GREEN: Color32 = Color32::from_rgb(0x34, 0xD3, 0x59);
const ERR_RED: Color32 = Color32::from_rgb(0xFF, 0x5B, 0x50);
const MAX_CONSOLE: usize = 200_000;
const NUCLEO_GREEN: Color32 = Color32::from_rgb(0x2E, 0xE0, 0x5A);
const NUCLEO_BLUE: Color32 = Color32::from_rgb(0x2F, 0x7B, 0xFF);
const NUCLEO_RED: Color32 = Color32::from_rgb(0xFF, 0x2A, 0x22);

pub struct App {
    emu: EmuHandle,
    display: Option<egui::TextureHandle>,
    /// Display frame taken from the snapshot (buffers are swapped, so that the
    /// emulator thread does not wait while the texture is made).
    display_frame: Vec<u8>,
    /// Board drawing; rendered at the size it is shown at.
    board: Option<board::Textures>,
    display_generation: u64,
    /// Buttons S0..S7 latched by right click.
    latched: [bool; 8],
    /// Pressed state last sent to the emulator.
    sent_buttons: [bool; 8],
    sent_user: bool,
    sent_touch: Option<(f32, f32)>,
    paused: bool,
    speed: Speed,
    console: String,
    console_input: String,
    /// Levels set on the AUX0–5 pads (`None` = open).
    ints: [Option<bool>; 6],
    analog: [Option<f32>; 8],
    self_screenshot: Option<SelfScreenshot>,
    /// The toolbar switch of the ST-LINK.
    usb_on: bool,
    settings: Settings,
}

/// Saves a picture of the own window and quits (`ITS_EMU_GUI_SCREENSHOT=file.png[:seconds]`).
/// Used for documentation and automated visual checks.
struct SelfScreenshot {
    path: PathBuf,
    at: std::time::Instant,
    requested: bool,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SelfScreenshot {
    fn from_env() -> Option<Self> {
        let spec = std::env::var("ITS_EMU_GUI_SCREENSHOT").ok()?;
        let (path, secs) = match spec.rsplit_once(':') {
            Some((p, s)) if s.parse::<f64>().is_ok() => (p.to_owned(), s.parse().unwrap()),
            _ => (spec, 3.0),
        };
        Some(Self {
            path: path.into(),
            at: std::time::Instant::now() + std::time::Duration::from_secs_f64(secs),
            requested: false,
            done: Default::default(),
        })
    }

    fn tick(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        if self.done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.requested && std::time::Instant::now() >= self.at {
            self.requested = true;
            let (path, done, repaint) = (self.path.clone(), self.done.clone(), ctx.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                if let Err(e) = save_png(&path, &img) {
                    log::error!("screenshot: {e}");
                }
                done.store(true, Ordering::SeqCst);
                repaint.request_repaint();
            })));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    enc.write_header()?.write_image_data(&bytes)?;
    Ok(())
}

/// Details of the ST-LINK state for tooltips.
fn usb_hover(usb: &UsbStatus) -> String {
    match usb {
        UsbStatus::Off => "The board's ST-LINK (debug probe + virtual COM port) as a USB device of this \
             computer, for pyOCD (the course's STLink@pyOCD configurations), OpenOCD and terminal programs. \
             Switch on to plug it in."
            .to_owned(),
        UsbStatus::PluggedIn { addr, debugging } => format!(
            "Plugged in over USB/IP ({addr}).{}",
            if *debugging {
                " A debugger (pyOCD) uses it; the emulator's GDB server leaves port 3333 to it."
            } else {
                ""
            }
        ),
        UsbStatus::Unplugged { addr, cmd, error } => {
            let why = error.as_ref().map_or_else(String::new, |e| format!("Attaching failed: {e}\n\n"));
            format!("{why}Exported over USB/IP on {addr}. Plug it in with\n{cmd}\n(needs the vhci-hcd kernel module)")
        }
        UsbStatus::Unavailable(e) => format!("The USB/IP server cannot start: {e}"),
    }
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext,
        firmware: Option<PathBuf>,
        gdb_addr: Option<String>,
        usb: emu::UsbConfig,
        settings: Settings,
        ideal_colors: bool,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let usb_on = usb.enabled;
        let emu = emu::spawn(move || ctx.request_repaint(), gdb_addr, usb);
        if ideal_colors {
            emu.send(Command::IdealColors(true));
        }
        if let Some(path) = firmware {
            emu.send(Command::Load(path));
        }
        Self {
            emu,
            display: None,
            display_frame: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
            board: None,
            display_generation: u64::MAX,
            latched: [false; 8],
            sent_buttons: [false; 8],
            sent_user: false,
            sent_touch: None,
            paused: false,
            speed: Speed::Factor(1.0),
            console: String::new(),
            console_input: String::new(),
            ints: [None; 6],
            analog: [None; 8],
            self_screenshot: SelfScreenshot::from_env(),
            usb_on,
            settings,
        }
    }

    fn open_dialog(&self) {
        let file =
            rfd::FileDialog::new().add_filter("Firmware", &["axf", "elf"]).add_filter("All files", &["*"]).pick_file();
        if let Some(path) = file {
            self.emu.send(Command::Load(path));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ---- take the latest snapshot
        let mut new_frame = false;
        let (state, firmware, led_duty, inputs, nucleo_duty, virtual_ns, core_mhz, speed, gdb, usb, dac1) = {
            let mut snap = self.emu.snapshot.lock().unwrap();
            if snap.display_generation != self.display_generation {
                self.display_generation = snap.display_generation;
                std::mem::swap(&mut snap.display, &mut self.display_frame);
                new_frame = true;
            }
            if !snap.serial.is_empty() {
                self.console.push_str(&String::from_utf8_lossy(&snap.serial));
                snap.serial.clear();
                if self.console.len() > MAX_CONSOLE {
                    let cut = self.console.len() - MAX_CONSOLE / 2;
                    let cut = (cut..self.console.len()).find(|&i| self.console.is_char_boundary(i)).unwrap_or(0);
                    self.console.drain(..cut);
                }
            }
            (
                snap.state.clone(),
                snap.firmware.clone(),
                snap.led_duty,
                snap.inputs,
                snap.nucleo_duty,
                snap.virtual_ns,
                snap.core_mhz,
                snap.speed,
                (snap.gdb.clone(), snap.gdb_attached),
                snap.usb.clone(),
                snap.dac1,
            )
        };

        if new_frame {
            let image = egui::ColorImage::from_rgba_unmultiplied([SCREEN_WIDTH, SCREEN_HEIGHT], &self.display_frame);
            match &mut self.display {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => self.display = Some(ctx.load_texture("display", image, egui::TextureOptions::LINEAR)),
            }
        }

        // ---- toolbar
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open…").clicked() {
                    self.open_dialog();
                }
                if ui.button("⟲ Reset").on_hover_text("RESET button of the Nucleo board").clicked() {
                    self.emu.send(Command::Reset);
                }
                let pause_label = if self.paused { "▶ Run" } else { "⏸ Pause" };
                if ui.button(pause_label).clicked() {
                    self.paused = !self.paused;
                    self.emu.send(Command::SetPaused(self.paused));
                }
                ui.separator();
                ui.label("Speed:");
                for (label, s) in [
                    ("1×", Speed::Factor(1.0)),
                    ("0.1×", Speed::Factor(0.1)),
                    ("10×", Speed::Factor(10.0)),
                    ("max", Speed::Max),
                ] {
                    if ui.selectable_label(self.speed == s, label).clicked() {
                        self.speed = s;
                        self.emu.send(Command::SetSpeed(s));
                    }
                }
                ui.separator();
                let mut on = self.usb_on;
                let switch = ui.toggle_value(&mut on, "USB ST-LINK").on_hover_text(usb_hover(&usb));
                if switch.changed() {
                    self.usb_on = on;
                    self.emu.send(Command::SetUsb(on));
                    self.settings.usb = on;
                    self.settings.save();
                }
                ui.separator();
                // The Nucleo user LEDs are hidden under the display on the real board.
                for (i, (name, pin, color)) in
                    [("LD1", "PB0", NUCLEO_GREEN), ("LD2", "PB7", NUCLEO_BLUE), ("LD3", "PB14", NUCLEO_RED)]
                        .into_iter()
                        .enumerate()
                {
                    let duty = nucleo_duty[i];
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    let b = duty.clamp(0.0, 1.0).powf(1.0 / 2.2);
                    ui.painter().circle_filled(rect.center(), 5.5, Color32::from_gray(60).lerp_to_gamma(color, b));
                    resp.on_hover_text(format!(
                        "Nucleo {name} ({pin}): {:.0} % — under the display on the real board",
                        duty * 100.0
                    ));
                    ui.label(name);
                }
            });
        });

        // ---- status bar
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let (text, color) = match &state {
                    RunState::NoFirmware => (
                        "flash empty — open an .axf file or load one with the debugger".to_owned(),
                        ui.visuals().weak_text_color(),
                    ),
                    RunState::Running => ("running".to_owned(), OK_GREEN),
                    RunState::Paused => ("paused".to_owned(), Color32::YELLOW),
                    RunState::Halted => ("halted by debugger".to_owned(), Color32::YELLOW),
                    RunState::Stopped(why) => (format!("stopped: {why}"), ERR_RED),
                    RunState::Error(e) => (e.clone(), ERR_RED),
                };
                ui.label(RichText::new(text).color(color));
                ui.separator();
                if let Some(name) = &firmware {
                    ui.label(name);
                    ui.separator();
                }
                ui.label(format!("t = {:.3} s", virtual_ns as f64 / 1e9));
                ui.separator();
                ui.label(format!("{core_mhz} MHz"));
                ui.separator();
                ui.label(format!("{speed:.2}× real time"));
                ui.separator();
                match &gdb {
                    (Ok(addr), true) => ui.label(RichText::new(format!("GDB attached ({addr})")).color(OK_GREEN)),
                    (Ok(addr), false) => ui.label(format!("GDB: {addr}")),
                    (Err(e), _) => ui.label(RichText::new(format!("GDB: {e}")).color(ui.visuals().weak_text_color())),
                };
                let (text, color) = match &usb {
                    UsbStatus::Off => (None, OK_GREEN),
                    UsbStatus::PluggedIn { debugging: true, .. } => (Some("ST-LINK in use by a debugger"), OK_GREEN),
                    UsbStatus::PluggedIn { .. } => (Some("ST-LINK plugged in"), OK_GREEN),
                    UsbStatus::Unplugged { error: None, .. } => {
                        (Some("ST-LINK: waiting for usbip attach"), Color32::YELLOW)
                    }
                    UsbStatus::Unplugged { error: Some(_), .. } | UsbStatus::Unavailable(_) => {
                        (Some("ST-LINK: not attached"), ERR_RED)
                    }
                };
                if let Some(text) = text {
                    ui.separator();
                    ui.label(RichText::new(text).color(color)).on_hover_text(usb_hover(&usb));
                }
            });
        });

        // ---- serial console
        egui::Panel::bottom("console").resizable(true).default_size(180.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Virtual COM port (USART3, 115200 8N1)").strong());
                if ui.small_button("clear").clicked() {
                    self.console.clear();
                }
            });
            let input_height = 28.0;
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .max_height(ui.available_height() - input_height)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&self.console).monospace()).wrap());
                });
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.console_input)
                        .hint_text("type and press Enter to send")
                        .desired_width(f32::INFINITY),
                );
                if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let mut data = std::mem::take(&mut self.console_input).into_bytes();
                    data.push(b'\n');
                    self.emu.send(Command::Serial(data));
                    edit.request_focus();
                }
            });
        });

        // ---- the board
        let typing = ctx.memory(|m| m.focused().is_some());
        let input = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_gray(28)).inner_margin(10.0))
            .show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    let tex = self.board.get_or_insert_with(|| board::Textures::load(ui.ctx()));
                    board::show(
                        ui,
                        tex,
                        &BoardState {
                            led_duty: &led_duty,
                            inputs,
                            display: self.display.as_ref(),
                            ints: &self.ints,
                            latched: &self.latched,
                            analog: &self.analog,
                            dac1,
                        },
                    )
                })
                .inner
            })
            .inner;

        const KEYS: [Key; 8] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7];
        for (i, key) in KEYS.into_iter().enumerate() {
            if input.latch_toggled[i] {
                self.latched[i] = !self.latched[i];
            }
            let down = input.buttons[i] || self.latched[i] || (!typing && ctx.input(|inp| inp.key_down(key)));
            if down != self.sent_buttons[i] {
                self.sent_buttons[i] = down;
                self.emu.send(Command::Button(i as u32, down));
            }
        }
        if input.user != self.sent_user {
            self.sent_user = input.user;
            self.emu.send(Command::UserButton(input.user));
        }
        if input.reset_clicked {
            self.emu.send(Command::Reset);
        }
        if input.touch != self.sent_touch {
            self.sent_touch = input.touch;
            self.emu.send(Command::Touch(input.touch));
        }
        if let Some(n) = input.int_clicked {
            self.ints[n] = match self.ints[n] {
                None => Some(false),
                Some(false) => Some(true),
                Some(true) => None,
            };
            self.emu.send(Command::Int(n as u32, self.ints[n]));
        }
        if let Some((n, volts)) = input.analog {
            self.analog[n] = volts;
            self.emu.send(Command::Analog(n, volts));
        }

        if let Some(shot) = &mut self.self_screenshot {
            shot.tick(&ctx);
        }

        // Drag & drop of firmware files
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.emu.send(Command::Load(path));
        }
    }
}
