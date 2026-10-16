//! egui front end: a picture of the ITS-Board with working LEDs, buttons and
//! display, plus a serial console.

use std::path::PathBuf;

use eframe::egui::{self, Color32, Key, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use itsboard::{SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::emu::{self, Command, EmuHandle, RunState, Speed};

const BOARD_BLUE: Color32 = Color32::from_rgb(0x1B, 0x3A, 0x7A);
const SILK: Color32 = Color32::from_rgb(0xE8, 0xEC, 0xF4);
const LED_OFF: Color32 = Color32::from_rgb(0x55, 0x5B, 0x66);
const LED_RED: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x30);
const LED_GREEN: Color32 = Color32::from_rgb(0x34, 0xD3, 0x59);
const LED_BLUE: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
const MAX_CONSOLE: usize = 200_000;

pub struct App {
    emu: EmuHandle,
    display: Option<egui::TextureHandle>,
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
    scale: f32,
    self_screenshot: Option<SelfScreenshot>,
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

impl App {
    pub fn new(cc: &eframe::CreationContext, firmware: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let emu = emu::spawn(move || ctx.request_repaint());
        if let Some(path) = firmware {
            emu.send(Command::Load(path));
        }
        Self {
            emu,
            display: None,
            display_generation: u64::MAX,
            latched: [false; 8],
            sent_buttons: [false; 8],
            sent_user: false,
            sent_touch: None,
            paused: false,
            speed: Speed::Factor(1.0),
            console: String::new(),
            console_input: String::new(),
            scale: 1.5,
            self_screenshot: SelfScreenshot::from_env(),
        }
    }

    fn open_dialog(&self) {
        let file = rfd::FileDialog::new().add_filter("Firmware", &["axf", "elf"]).add_filter("All files", &["*"]).pick_file();
        if let Some(path) = file {
            self.emu.send(Command::Load(path));
        }
    }
}

/// Draws a round LED with a label below it.
fn led(ui: &mut egui::Ui, on: bool, color: Color32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(34.0, 30.0), Sense::hover());
    let center = Pos2::new(rect.center().x, rect.top() + 9.0);
    let painter = ui.painter();
    if on {
        painter.circle_filled(center, 11.0, color.gamma_multiply(0.25));
    }
    painter.circle_filled(center, 6.5, if on { color } else { LED_OFF });
    painter.circle_stroke(center, 6.5, Stroke::new(1.0, Color32::from_black_alpha(120)));
    painter.text(
        Pos2::new(rect.center().x, rect.bottom() - 4.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional(10.0),
        SILK,
    );
}

/// A tactile push button; returns (held down, right-clicked).
fn push_button(ui: &mut egui::Ui, label: &str, color: Color32, latched: bool) -> (bool, bool) {
    let (outer, response) = ui.allocate_exact_size(Vec2::new(34.0, 48.0), Sense::click_and_drag());
    let rect = Rect::from_min_size(outer.min, Vec2::new(34.0, 34.0));
    let down = response.is_pointer_button_down_on() || latched;
    let painter = ui.painter();
    painter.rect_filled(rect.shrink(2.0), 4.0, Color32::from_rgb(0x2B, 0x2F, 0x36));
    let cap = rect.shrink(if down { 8.0 } else { 6.0 });
    painter.circle_filled(cap.center(), cap.width() / 2.0, if down { color.gamma_multiply(0.6) } else { color });
    if latched {
        painter.circle_stroke(cap.center(), cap.width() / 2.0 + 2.0, Stroke::new(2.0, Color32::YELLOW));
    }
    painter.text(
        outer.center_bottom() - Vec2::new(0.0, 2.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional(10.0),
        SILK,
    );
    (down, response.secondary_clicked())
}

const BOARD_MARGIN: f32 = 16.0;
const IO_WIDTH: f32 = 8.0 * 42.0;
const NUCLEO_WIDTH: f32 = 130.0;
const GAP: f32 = 20.0;
const LCD_BEZEL: f32 = 16.0;

impl App {
    /// "Out" LEDs and "In" buttons as on the left half of the PCB.
    fn io_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, leds: u16, inputs: u8, typing: bool) {
        ui.vertical(|ui| {
            ui.set_width(IO_WIDTH);
            ui.label(RichText::new("Out").color(SILK).size(16.0));
            for row in [8u32, 0] {
                ui.horizontal(|ui| {
                    for i in (row..row + 8).rev() {
                        led(ui, leds & (1 << i) != 0, LED_RED, &format!("OUT{i}"));
                    }
                });
            }
            ui.add_space(14.0);
            ui.label(RichText::new("In").color(SILK).size(16.0));
            ui.horizontal(|ui| {
                for i in (0..8u32).rev() {
                    led(ui, inputs & (1 << i) != 0, LED_GREEN, &format!("IN{i}"));
                }
            });
            ui.horizontal(|ui| {
                for i in (0..8usize).rev() {
                    let (mut down, toggled) = push_button(ui, &format!("S{i}"), Color32::from_rgb(0xE5, 0x48, 0x5A), self.latched[i]);
                    if toggled {
                        self.latched[i] = !self.latched[i];
                    }
                    let key = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7][i];
                    down |= !typing && ctx.input(|inp| inp.key_down(key));
                    if down != self.sent_buttons[i] {
                        self.sent_buttons[i] = down;
                        self.emu.send(Command::Button(i as u32, down));
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("keys 0–7 = S0–S7 · right click latches a button").color(SILK.gamma_multiply(0.6)).size(10.0));
        });
    }

    /// The TFT; mouse presses are touches.
    fn display_section(&mut self, ui: &mut egui::Ui, scale: f32) {
        let size = Vec2::new(SCREEN_WIDTH as f32, SCREEN_HEIGHT as f32) * scale;
        let (rect, response) = ui.allocate_exact_size(size + Vec2::splat(LCD_BEZEL), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, Color32::from_gray(15));
        let screen = Rect::from_center_size(rect.center(), size);
        if let Some(tex) = &self.display {
            painter.image(tex.id(), screen, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        let to_screen = |p: Pos2| ((p.x - screen.left()) / scale, (p.y - screen.top()) / scale);
        let touch = response
            .is_pointer_button_down_on()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .filter(|p| screen.contains(*p))
            .map(to_screen);
        if touch != self.sent_touch {
            self.sent_touch = touch;
            self.emu.send(Command::Touch(touch));
        }
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if let Some((x, y)) = touch.or_else(|| response.hover_pos().filter(|p| screen.contains(*p)).map(to_screen)) {
            response.on_hover_text_at_pointer(format!("x = {:.0}, y = {:.0}", x.floor(), y.floor()));
        }
    }

    /// LEDs and buttons of the Nucleo board.
    fn nucleo_section(&mut self, ui: &mut egui::Ui, nucleo: [bool; 3]) {
        ui.vertical(|ui| {
            ui.set_width(NUCLEO_WIDTH);
            ui.label(RichText::new("NUCLEO-F429ZI").color(SILK).size(14.0));
            ui.horizontal(|ui| {
                led(ui, nucleo[0], LED_GREEN, "LD1");
                led(ui, nucleo[1], LED_BLUE, "LD2");
                led(ui, nucleo[2], LED_RED, "LD3");
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let (down, _) = push_button(ui, "USER", LED_BLUE, false);
                if down != self.sent_user {
                    self.sent_user = down;
                    self.emu.send(Command::UserButton(down));
                }
                let (reset, _) = push_button(ui, "RESET", Color32::from_gray(20), false);
                if reset && ui.input(|i| i.pointer.primary_pressed()) {
                    self.emu.send(Command::Reset);
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ---- take the latest snapshot
        let (state, firmware, leds, inputs, nucleo, virtual_ns, core_mhz, speed) = {
            let mut snap = self.emu.snapshot.lock().unwrap();
            if snap.display_generation != self.display_generation {
                self.display_generation = snap.display_generation;
                let image = egui::ColorImage::from_rgba_unmultiplied([SCREEN_WIDTH, SCREEN_HEIGHT], &snap.display);
                match &mut self.display {
                    Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                    None => self.display = Some(ctx.load_texture("display", image, egui::TextureOptions::NEAREST)),
                }
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
                snap.leds,
                snap.inputs,
                snap.nucleo_leds,
                snap.virtual_ns,
                snap.core_mhz,
                snap.speed,
            )
        };

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
                for (label, s) in [("1×", Speed::Factor(1.0)), ("0.1×", Speed::Factor(0.1)), ("10×", Speed::Factor(10.0)), ("max", Speed::Max)] {
                    if ui.selectable_label(self.speed == s, label).clicked() {
                        self.speed = s;
                        self.emu.send(Command::SetSpeed(s));
                    }
                }
                ui.separator();
                ui.label("Max. display zoom:");
                ui.add(egui::Slider::new(&mut self.scale, 1.0..=3.0).step_by(0.25));
            });
        });

        // ---- status bar
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let (text, color) = match &state {
                    RunState::NoFirmware => ("no firmware loaded — open an .axf file".to_owned(), ui.visuals().weak_text_color()),
                    RunState::Running => ("running".to_owned(), LED_GREEN),
                    RunState::Paused => ("paused".to_owned(), Color32::YELLOW),
                    RunState::Stopped(why) => (format!("stopped: {why}"), LED_RED),
                    RunState::Error(e) => (e.clone(), LED_RED),
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
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_gray(30)).inner_margin(8.0)).show(ui, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                // Side by side if the display fits at a reasonable size, else stacked.
                let avail = ui.available_width() - 2.0 * BOARD_MARGIN;
                let side_scale = (avail - IO_WIDTH - NUCLEO_WIDTH - 2.0 * GAP - LCD_BEZEL) / SCREEN_WIDTH as f32;
                let stacked = side_scale < 0.9;
                let fit = if stacked { (avail - LCD_BEZEL) / SCREEN_WIDTH as f32 } else { side_scale };
                let scale = self.scale.min(fit).max(0.5);
                egui::Frame::new().fill(BOARD_BLUE).corner_radius(10.0).inner_margin(BOARD_MARGIN).show(ui, |ui| {
                    if stacked {
                        ui.horizontal_top(|ui| {
                            self.io_section(ui, &ctx, leds, inputs, typing);
                            ui.add_space(GAP);
                            self.nucleo_section(ui, nucleo);
                        });
                        ui.add_space(GAP);
                        self.display_section(ui, scale);
                    } else {
                        ui.horizontal_top(|ui| {
                            self.io_section(ui, &ctx, leds, inputs, typing);
                            ui.add_space(GAP);
                            self.display_section(ui, scale);
                            ui.add_space(GAP);
                            self.nucleo_section(ui, nucleo);
                        });
                    }
                });
            });
        });

        if let Some(shot) = &mut self.self_screenshot {
            shot.tick(&ctx);
        }

        // Drag & drop of firmware files
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(path) = dropped.into_iter().next() {
            self.emu.send(Command::Load(path));
        }
    }
}
