//! egui front end: a picture of the ITS-Board with working LEDs, buttons and
//! display, plus a serial console.

use std::path::PathBuf;

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use itsboard::{SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::board::{self, BoardState, NUCLEO_BLUE, NUCLEO_GREEN, NUCLEO_RED};
use crate::console::Console;
use crate::emu::{self, Command, EmuHandle, RunState, Speed, UsbStatus};
use crate::files::FilePicker;
use crate::settings::{ConsoleSide, Settings, Theme, WindowState};

const OK_GREEN: Color32 = Color32::from_rgb(0x34, 0xD3, 0x59);
const ERR_RED: Color32 = Color32::from_rgb(0xFF, 0x5B, 0x50);
const WARN_YELLOW: Color32 = Color32::from_rgb(0xFF, 0xD5, 0x4A);

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const RESET: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const DISPLAY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::D);
const PINS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::P);
const SEE_THROUGH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
const DEVICES: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::E);
const CONSOLE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::L);
const QUIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Q);
const RUN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Space);
const ZOOM_RESET: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Home);
const HELP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F1);

const SPEEDS: [(&str, Speed); 4] =
    [("0.1×", Speed::Factor(0.1)), ("1×", Speed::Factor(1.0)), ("10×", Speed::Factor(10.0)), ("max", Speed::Max)];

/// How long a notice stays in the status bar (seconds).
const NOTICE_TIME: f64 = 8.0;
/// Sizes for choosing where the console goes: its width on the right, its height below.
const CONSOLE_W: f32 = 380.0;
const CONSOLE_H: f32 = 150.0;
/// Width of the Devices panel at the start.
const DEVICES_W: f32 = 340.0;

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
    speed: Speed,
    console: Console,
    /// The console is right of the board (else below it).
    console_right: bool,
    /// Levels set on the AUX0–5 pads (`None` = open).
    ints: [Option<bool>; 6],
    analog: [Option<f32>; 8],
    self_screenshot: Option<SelfScreenshot>,
    /// The toolbar switch of the ST-LINK.
    usb_on: bool,
    settings: Settings,
    /// Only the display, as large as the window allows (right click on the display).
    lcd_focus: bool,
    /// The Nucleo and the display see-through, with this opacity.
    see_through: Option<f32>,
    /// Opacity see-through starts with (the slider's last value).
    see_through_opacity: f32,
    /// Filter the display texture was made with (sharp pixels when it is shown alone).
    display_options: egui::TextureOptions,
    /// The display shows the frame buffer's colours instead of the real panel's look.
    ideal_colors: bool,
    /// Window title last set.
    title: String,
    /// The pin inspector is open.
    show_pins: bool,
    /// The Devices panel is open.
    show_devices: bool,
    devices: crate::devices::DevicesPanel,
    /// The help window is open.
    show_help: bool,
    /// Message in the status bar: text, error, until (egui time).
    notice: Option<(String, bool, f64)>,
    files: FilePicker,
    /// A text field had the keyboard focus at the end of the last frame (egui
    /// drops the focus on Esc before the frame, so this tells an Esc that
    /// left the field from one meant for the board).
    text_was_focused: bool,
    /// Zoom and pan of the board drawing.
    board_view: board::Zoom,
    /// The window state last saved.
    saved_window: Option<WindowState>,
}

/// Speed as the settings keep it.
fn speed_value(speed: Speed) -> f64 {
    match speed {
        Speed::Factor(f) => f,
        Speed::Max => 0.0,
    }
}

fn speed_from(value: f64) -> Speed {
    SPEEDS.iter().map(|(_, s)| *s).find(|s| speed_value(*s) == value).unwrap_or(Speed::Factor(1.0))
}

const SEE_THROUGH_DEFAULT: f32 = 0.25;

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

/// `ITS_EMU_GUI_VIEW` names this view.
fn start_view(name: &str) -> bool {
    std::env::var("ITS_EMU_GUI_VIEW").is_ok_and(|v| v.split(',').any(|v| v.trim() == name))
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

/// The Run/Pause button for `state`: label, enabled, tooltip.
fn run_button(state: &RunState) -> (&'static str, bool, &'static str) {
    match state {
        RunState::Running => ("Pause", true, "Pause the emulation · Space"),
        RunState::Paused => ("Run", true, "Continue the emulation · Space"),
        RunState::Halted => ("Run", false, "Halted by the debugger: continue in the debugger"),
        _ => ("Run", false, "Nothing is running: open a firmware file or press Reset"),
    }
}

/// A menu entry with its shortcut.
fn menu_item(ui: &mut egui::Ui, text: &str, shortcut: &KeyboardShortcut) -> bool {
    let shortcut = ui.ctx().format_shortcut(shortcut);
    ui.add(egui::Button::new(text).shortcut_text(shortcut)).clicked()
}

/// A checkable menu entry with its shortcut.
fn menu_toggle(ui: &mut egui::Ui, on: &mut bool, text: &str, shortcut: Option<&KeyboardShortcut>) -> bool {
    let mut button = egui::Button::selectable(*on, text);
    if let Some(s) = shortcut {
        button = button.shortcut_text(ui.ctx().format_shortcut(s));
    }
    let clicked = ui.add(button).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext,
        firmware: Option<PathBuf>,
        gdb_addr: Option<String>,
        usb: emu::UsbConfig,
        bootrom_override: Option<PathBuf>,
        settings: Settings,
        ideal_colors: bool,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        apply_theme(&ctx, settings.theme);
        let usb_on = usb.enabled;
        let bootrom = bootrom_override.clone().or_else(|| settings.pico_bootrom.clone());
        let emu = emu::spawn(move || ctx.request_repaint(), gdb_addr, usb, settings.devices.clone(), bootrom);
        if ideal_colors {
            emu.send(Command::IdealColors(true));
        }
        if !settings.auto_reload {
            emu.send(Command::SetAutoReload(false));
        }
        emu.send(Command::SetBounce(settings.bounce));
        let speed = speed_from(settings.speed);
        if speed != Speed::Factor(1.0) {
            emu.send(Command::SetSpeed(speed));
        }
        if start_view("pins") {
            emu.send(Command::ShowPins(true));
        }
        // the file dialog starts in the folder of the last opened file
        let files = FilePicker::new(settings.recent.first().and_then(|p| p.parent()).map(PathBuf::from));
        let mut app = Self {
            emu,
            display: None,
            display_frame: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
            board: None,
            display_generation: u64::MAX,
            latched: [false; 8],
            sent_buttons: [false; 8],
            sent_user: false,
            sent_touch: None,
            speed,
            console: Console::new(settings.line_ending, settings.echo, settings.timestamps),
            console_right: false,
            ints: [None; 6],
            analog: [None; 8],
            self_screenshot: SelfScreenshot::from_env(),
            usb_on,
            settings,
            // ITS_EMU_GUI_VIEW=lcd, see-through, pins, help (comma separated):
            // start in that view (documentation, visual checks)
            lcd_focus: start_view("lcd"),
            see_through: start_view("see-through").then_some(SEE_THROUGH_DEFAULT),
            see_through_opacity: SEE_THROUGH_DEFAULT,
            display_options: egui::TextureOptions::LINEAR,
            ideal_colors,
            title: String::new(),
            show_pins: start_view("pins"),
            show_devices: start_view("devices"),
            devices: crate::devices::DevicesPanel::new(bootrom_override),
            show_help: start_view("help"),
            notice: None,
            files,
            text_was_focused: false,
            board_view: board::Zoom::default(),
            saved_window: None,
        };
        if let Some(path) = firmware {
            app.load(path);
        }
        app
    }

    /// Loads a firmware file and remembers it among the recent ones.
    fn load(&mut self, path: PathBuf) {
        self.settings.add_recent(&path);
        self.settings.save();
        self.emu.send(Command::Load(path));
    }

    fn notify(&mut self, ctx: &egui::Context, text: String, error: bool) {
        if error {
            log::error!("{text}");
        }
        self.notice = Some((text, error, ctx.input(|i| i.time) + NOTICE_TIME));
    }

    /// Saves the display as it is shown (480 × 320 pixels) to a PNG file the user picks.
    fn save_display_dialog(&mut self, ctx: &egui::Context) {
        let file = rfd::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name("display.png").save_file();
        if let Some(path) = file {
            let img = egui::ColorImage::from_rgba_unmultiplied([SCREEN_WIDTH, SCREEN_HEIGHT], &self.display_frame);
            match save_png(&path, &img) {
                Ok(()) => self.notify(ctx, format!("Display saved to {}", path.display()), false),
                Err(e) => self.notify(ctx, format!("Cannot save {}: {e}", path.display()), true),
            }
        }
    }

    /// Saves the text of the serial console to a file the user picks.
    fn save_console_dialog(&mut self, ctx: &egui::Context) {
        let file = rfd::FileDialog::new().add_filter("Text", &["txt", "log"]).set_file_name("serial.txt").save_file();
        if let Some(path) = file {
            match std::fs::write(&path, self.console.text()) {
                Ok(()) => self.notify(ctx, format!("Serial console saved to {}", path.display()), false),
                Err(e) => self.notify(ctx, format!("Cannot save {}: {e}", path.display()), true),
            }
        }
    }

    fn toggle_run(&self, state: &RunState) {
        if run_button(state).1 {
            self.emu.send(Command::SetPaused(*state == RunState::Running));
        }
    }

    fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
        self.emu.send(Command::SetSpeed(speed));
        self.settings.speed = speed_value(speed);
        self.settings.save();
    }

    fn set_pins(&mut self, on: bool) {
        self.show_pins = on;
        self.emu.send(Command::ShowPins(on));
    }

    /// Remembers the window's size and position when they change.
    fn save_window(&mut self, ctx: &egui::Context) {
        let state = ctx.input(|i| {
            let v = i.viewport();
            let size = v.inner_rect.map(|r| [r.width(), r.height()])?;
            let pos = v.outer_rect.map(|r| [r.min.x, r.min.y]);
            Some(WindowState { size, pos, maximized: v.maximized.unwrap_or(false) })
        });
        if let Some(state) = state
            && Some(state) != self.saved_window
        {
            // the first frames of a window may still be resized by the window manager
            if self.saved_window.is_some() || ctx.cumulative_frame_nr() > 10 {
                self.saved_window = Some(state);
                self.settings.window = Some(state);
                self.settings.save();
            }
        }
    }

    fn set_usb(&mut self, on: bool) {
        self.usb_on = on;
        self.emu.send(Command::SetUsb(on));
        self.settings.usb = on;
        self.settings.save();
    }

    fn set_see_through(&mut self, on: bool) {
        if let Some(opacity) = self.see_through {
            self.see_through_opacity = opacity;
        }
        self.see_through = on.then_some(self.see_through_opacity);
    }

    fn set_console(&mut self, on: bool) {
        self.settings.console = on;
        self.settings.save();
    }

    /// Where the console goes: right of the board when the board is shown larger that way.
    fn place_console(&mut self, ctx: &egui::Context) {
        self.console_right = match self.settings.console_side {
            ConsoleSide::Bottom => false,
            ConsoleSide::Right => true,
            ConsoleSide::Auto => {
                let r = ctx.content_rect();
                let pins = if self.show_pins { crate::pins::MIN_WIDTH + 20.0 } else { 0.0 };
                let devices = if self.show_devices { DEVICES_W } else { 0.0 };
                let (w, h) = (r.width() - pins - devices - 20.0, r.height() - 100.0);
                let scale = |w: f32, h: f32| (w / board::BOARD_W).min(h / board::BOARD_H);
                let below = scale(w, h - CONSOLE_H);
                let right = scale(w - CONSOLE_W, h);
                // a little hysteresis, so that it does not jump to and fro while resizing
                if self.console_right { right > below * 0.95 } else { right > below * 1.05 }
            }
        };
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, state: &RunState) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if menu_item(ui, "Open firmware…", &OPEN) {
                    self.files.open(ctx, false);
                }
                if ui.button("Open with the built-in file browser…").clicked() {
                    self.files.open(ctx, true);
                }
                let mut open = None;
                ui.add_enabled_ui(!self.settings.recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for path in &self.settings.recent {
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            let dir = path.parent().and_then(|d| d.file_name()).unwrap_or_default();
                            let label = format!("{name}  ({})", dir.to_string_lossy());
                            if ui.button(label).on_hover_text(path.display().to_string()).clicked() {
                                open = Some(path.clone());
                            }
                        }
                    });
                });
                if let Some(path) = open {
                    self.load(path);
                }
                ui.separator();
                if menu_item(ui, "Save display as PNG…", &SAVE) {
                    self.save_display_dialog(ctx);
                }
                if ui.button("Save serial console…").clicked() {
                    self.save_console_dialog(ctx);
                }
                ui.separator();
                if menu_item(ui, "Quit", &QUIT) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Emulation", |ui| {
                let (label, enabled, _) = run_button(state);
                let shortcut = ctx.format_shortcut(&RUN);
                if ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut)).clicked() {
                    self.toggle_run(state);
                }
                if menu_item(ui, "Reset", &RESET) {
                    self.emu.send(Command::Reset);
                }
                ui.separator();
                ui.label(RichText::new("Speed").weak());
                for (label, s) in SPEEDS {
                    if ui.radio(self.speed == s, label).clicked() {
                        self.set_speed(s);
                    }
                }
                ui.separator();
                let mut on = self.usb_on;
                if ui.checkbox(&mut on, "USB ST-LINK plugged in").changed() {
                    self.set_usb(on);
                }
                let mut bounce = self.settings.bounce;
                if ui
                    .checkbox(&mut bounce, "Contact bounce of the buttons")
                    .on_hover_text(
                        "The buttons S0–S7, USER and the encoder contacts bounce for a moment when pressed \
                         or released, as on the real board: a program without debouncing sees several edges",
                    )
                    .changed()
                {
                    self.settings.bounce = bounce;
                    self.settings.save();
                    self.emu.send(Command::SetBounce(bounce));
                }
                let mut auto = self.settings.auto_reload;
                if ui
                    .checkbox(&mut auto, "Reload the firmware when it is rebuilt")
                    .on_hover_text(
                        "Load the .axf file again (and restart) when a new build replaces it — \
                         not while a debugger is attached",
                    )
                    .changed()
                {
                    self.settings.auto_reload = auto;
                    self.settings.save();
                    self.emu.send(Command::SetAutoReload(auto));
                }
            });
            ui.menu_button("View", |ui| {
                menu_toggle(ui, &mut self.lcd_focus, "Display alone", Some(&DISPLAY));
                let mut see = self.see_through.is_some();
                if ui
                    .add_enabled_ui(!self.lcd_focus, |ui| menu_toggle(ui, &mut see, "See-through", Some(&SEE_THROUGH)))
                    .inner
                {
                    self.set_see_through(see);
                }
                let mut pins = self.show_pins;
                if menu_toggle(ui, &mut pins, "Pins", Some(&PINS)) {
                    self.set_pins(pins);
                }
                menu_toggle(ui, &mut self.show_devices, "Devices", Some(&DEVICES));
                let mut console = self.settings.console;
                if menu_toggle(ui, &mut console, "Serial console", Some(&CONSOLE)) {
                    self.set_console(console);
                }
                ui.menu_button("Serial console position", |ui| {
                    for (side, label) in [
                        (ConsoleSide::Auto, "Automatic (right in wide windows)"),
                        (ConsoleSide::Bottom, "Below the board"),
                        (ConsoleSide::Right, "Right of the board"),
                    ] {
                        if ui.radio(self.settings.console_side == side, label).clicked() {
                            self.settings.console_side = side;
                            self.settings.save();
                        }
                    }
                });
                ui.separator();
                let mut real = !self.ideal_colors;
                if ui
                    .checkbox(&mut real, "Display colours as on the real panel")
                    .on_hover_text(
                        "The measured tone response of the board's display (darker mid tones, grey black); \
                         off: the frame buffer's colours",
                    )
                    .changed()
                {
                    self.ideal_colors = !real;
                    self.emu.send(Command::IdealColors(self.ideal_colors));
                }
                ui.separator();
                ui.menu_button("Theme", |ui| {
                    for (theme, label) in [(Theme::System, "System"), (Theme::Dark, "Dark"), (Theme::Light, "Light")] {
                        if ui.radio(self.settings.theme == theme, label).clicked() {
                            self.settings.theme = theme;
                            self.settings.save();
                            apply_theme(ctx, theme);
                        }
                    }
                });
                if ui
                    .add(egui::Button::new("Board zoom: fit").shortcut_text(ctx.format_shortcut(&ZOOM_RESET)))
                    .on_hover_text("Ctrl + mouse wheel zooms the board at the pointer, dragging with the middle button or Shift pans it")
                    .clicked()
                {
                    self.board_view = board::Zoom::default();
                }
                egui::gui_zoom::zoom_menu_buttons(ui);
            });
            ui.menu_button("Help", |ui| {
                if menu_item(ui, "Controls and mouse actions", &HELP) {
                    self.show_help = true;
                }
            });
        });
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        state: &RunState,
        usb: &UsbStatus,
        nucleo: [f32; 3],
        devices: usize,
    ) {
        ui.horizontal_wrapped(|ui| {
            let open = ui.button("Open…").on_hover_text(format!(
                "Open a firmware file (.axf) · {}\nShift+click: the emulator's own file browser",
                ctx.format_shortcut(&OPEN)
            ));
            if open.clicked() {
                self.files.open(ctx, ui.input(|i| i.modifiers.shift));
            }
            if ui
                .button("Reset")
                .on_hover_text(format!("RESET button of the Nucleo board · {}", ctx.format_shortcut(&RESET)))
                .clicked()
            {
                self.emu.send(Command::Reset);
            }
            let (label, enabled, hover) = run_button(state);
            // the same width for both labels, so that nothing moves
            let width = ui.spacing().interact_size.x.max(52.0);
            if ui
                .add_enabled(enabled, egui::Button::new(label).min_size(egui::vec2(width, 0.0)))
                .on_hover_text(hover)
                .on_disabled_hover_text(hover)
                .clicked()
            {
                self.toggle_run(state);
            }
            ui.separator();
            ui.label("Speed:");
            for (label, s) in SPEEDS {
                if ui.selectable_label(self.speed == s, label).clicked() {
                    self.set_speed(s);
                }
            }
            ui.separator();
            let mut on = self.usb_on;
            if ui.toggle_value(&mut on, "USB ST-LINK").on_hover_text(usb_hover(usb)).changed() {
                self.set_usb(on);
            }
            ui.separator();
            ui.toggle_value(&mut self.lcd_focus, "Display").on_hover_text(format!(
                "Only the display, as large as possible · {}, or right click on the display; Esc returns",
                ctx.format_shortcut(&DISPLAY)
            ));
            let see = self.see_through.is_some();
            if ui
                .add_enabled(!self.lcd_focus, egui::Button::selectable(see, "See-through"))
                .on_hover_text(format!(
                    "Show the Nucleo and the display see-through, to see the carrier board under them · {}",
                    ctx.format_shortcut(&SEE_THROUGH)
                ))
                .clicked()
            {
                self.set_see_through(!see);
            }
            // always there (greyed out when off), so that the buttons after it do not move
            let mut opacity = self.see_through.unwrap_or(self.see_through_opacity);
            let slider = ui
                .scope(|ui| {
                    ui.spacing_mut().slider_width = 64.0;
                    ui.add_enabled(see && !self.lcd_focus, egui::Slider::new(&mut opacity, 0.0..=1.0).show_value(false))
                })
                .inner
                .on_hover_text("Opacity of the Nucleo and the display");
            if slider.changed()
                && let Some(o) = &mut self.see_through
            {
                *o = opacity;
            }
            let mut pins = self.show_pins;
            if ui
                .toggle_value(&mut pins, "Pins")
                .on_hover_text(format!("Mode and level of every pin of ports A–G · {}", ctx.format_shortcut(&PINS)))
                .changed()
            {
                self.set_pins(pins);
            }
            let label = match devices {
                0 => "Devices".to_owned(),
                n => format!("Devices ({n})"),
            };
            ui.toggle_value(&mut self.show_devices, label).on_hover_text(format!(
                "Devices on the header pins: Raspberry Pi Pico, rotary encoder, DS18B20 · {}",
                ctx.format_shortcut(&DEVICES)
            ));
            let mut console = self.settings.console;
            if ui
                .toggle_value(&mut console, "Console")
                .on_hover_text(format!("Serial console (virtual COM port) · {}", ctx.format_shortcut(&CONSOLE)))
                .changed()
            {
                self.set_console(console);
            }
            ui.separator();
            // The Nucleo user LEDs are hidden under the display on the real board.
            ui.label(RichText::new("Nucleo").weak())
                .on_hover_text("The user LEDs of the Nucleo board, hidden under the display on the real board");
            for (i, (name, pin, color)) in
                [("LD1", "PB0", NUCLEO_GREEN), ("LD2", "PB7", NUCLEO_BLUE), ("LD3", "PB14", NUCLEO_RED)]
                    .into_iter()
                    .enumerate()
            {
                let duty = nucleo[i];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let b = duty.clamp(0.0, 1.0).powf(1.0 / 2.2);
                // an SMD LED with its glow, not a radio button or check box
                let p = ui.painter();
                if b > 0.01 {
                    p.circle_filled(rect.center(), 7.0, color.gamma_multiply(0.35 * b));
                }
                let body = egui::Rect::from_center_size(rect.center(), egui::vec2(10.0, 6.0));
                p.rect_filled(body, 1.5, Color32::from_gray(70).lerp_to_gamma(color, b));
                if b > 0.01 {
                    p.circle_filled(rect.center(), 2.0, Color32::WHITE.gamma_multiply(0.8 * b));
                }
                let text = format!("Nucleo {name} ({pin}): {:.0} %", duty * 100.0);
                let label = text.clone();
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
                resp.on_hover_text(text);
                ui.label(RichText::new(name).small());
            }
            ui.separator();
            if ui
                .button("?")
                .on_hover_text(format!("Controls and mouse actions · {}", ctx.format_shortcut(&HELP)))
                .clicked()
            {
                self.show_help = !self.show_help;
            }
        });
    }

    #[expect(clippy::too_many_arguments)]
    fn status_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &RunState,
        firmware: &Option<String>,
        virtual_ns: u64,
        core_mhz: u32,
        speed: f64,
        gdb: &(Result<String, String>, bool),
        usb: &UsbStatus,
    ) {
        ui.horizontal_wrapped(|ui| {
            let (text, color) = match state {
                RunState::NoFirmware => (
                    "no firmware — open an .axf file or load one with the debugger".to_owned(),
                    ui.visuals().weak_text_color(),
                ),
                RunState::Running => ("running".to_owned(), OK_GREEN),
                RunState::Paused => ("paused".to_owned(), WARN_YELLOW),
                RunState::Halted => ("halted by debugger".to_owned(), WARN_YELLOW),
                RunState::Stopped(why) => (format!("stopped: {why}"), ERR_RED),
                RunState::Error(e) => (e.clone(), ERR_RED),
            };
            ui.label(RichText::new(text).color(color));
            ui.separator();
            // time, clock and speed only mean something with a firmware
            if *state != RunState::NoFirmware {
                if let Some(name) = firmware {
                    ui.label(name);
                    ui.separator();
                }
                ui.label(format!("t = {:.3} s", virtual_ns as f64 / 1e9));
                ui.separator();
                ui.label(format!("{core_mhz} MHz"));
                ui.separator();
                let behind = matches!(self.speed, Speed::Factor(f) if *state == RunState::Running && speed < f * 0.9);
                let text = RichText::new(format!("{speed:.2}× real time"));
                if behind {
                    ui.label(text.color(WARN_YELLOW))
                        .on_hover_text("The computer cannot keep up with the chosen speed");
                } else {
                    ui.label(text);
                }
                ui.separator();
            }
            match gdb {
                (Ok(addr), true) => ui.label(RichText::new(format!("GDB attached ({addr})")).color(OK_GREEN)),
                (Ok(addr), false) => ui.label(format!("GDB: {addr}")),
                (Err(e), _) => ui.label(RichText::new(format!("GDB: {e}")).color(ui.visuals().weak_text_color())),
            };
            let (text, color) = match usb {
                UsbStatus::Off => (None, OK_GREEN),
                UsbStatus::PluggedIn { debugging: true, .. } => (Some("ST-LINK in use by a debugger"), OK_GREEN),
                UsbStatus::PluggedIn { .. } => (Some("ST-LINK plugged in"), OK_GREEN),
                UsbStatus::Unplugged { error: None, .. } => (Some("ST-LINK: waiting for usbip attach"), WARN_YELLOW),
                UsbStatus::Unplugged { error: Some(_), .. } if stlink::restart_needed() => {
                    (Some("ST-LINK: restart the computer (kernel updated)"), ERR_RED)
                }
                UsbStatus::Unplugged { error: Some(_), .. } | UsbStatus::Unavailable(_) => {
                    (Some("ST-LINK: not attached"), ERR_RED)
                }
            };
            if let Some(text) = text {
                ui.separator();
                ui.label(RichText::new(text).color(color)).on_hover_text(usb_hover(usb));
            }
            let now = ui.input(|i| i.time);
            if let Some((text, error, until)) = self.notice.clone() {
                if now < until {
                    ui.separator();
                    let color = if error { ERR_RED } else { ui.visuals().strong_text_color() };
                    let label = egui::Label::new(RichText::new(text).color(color)).sense(egui::Sense::click());
                    if ui.add(label).on_hover_text("Click to dismiss").clicked() {
                        self.notice = None;
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(until - now));
                } else {
                    self.notice = None;
                }
            }
        });
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_help;
        let key = |s: &KeyboardShortcut| ctx.format_shortcut(s);
        egui::Window::new("Controls")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                egui::Grid::new("controls").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                    let mut row = |what: &str, how: String| {
                        ui.label(what);
                        ui.label(RichText::new(how).strong());
                        ui.end_row();
                    };
                    row("Press S0–S7", "Click on a button · keys 0–7".into());
                    row("Latch S0–S7 (stays pressed)", "Right click on the button".into());
                    row("USER / RESET of the Nucleo", "Click on the blue / black button".into());
                    row("Touch the display", "Click or drag on the display".into());
                    row("Display alone, and back", format!("Right click on the display · {} · Esc", key(&DISPLAY)));
                    row("AUX0–5: open → GND → 3V3", "Click on the pad".into());
                    row("Voltage on an Analog pad", "Mouse wheel (±0.05 V) or right click on the pad".into());
                    row("Load a firmware file", format!("Drop it onto the window · {} · File menu", key(&OPEN)));
                    row("Run / pause", key(&RUN));
                    row("Reset", key(&RESET));
                    row("Save the display as PNG", key(&SAVE));
                    row(
                        "See-through · pins · devices · console",
                        format!("{} · {} · {} · {}", key(&SEE_THROUGH), key(&PINS), key(&DEVICES), key(&CONSOLE)),
                    );
                    row(
                        "Zoom the board · pan · back to fit",
                        format!("Ctrl + wheel · middle button or Shift + drag · {}", key(&ZOOM_RESET)),
                    );
                    row("Zoom the whole window", "Ctrl + / Ctrl − / Ctrl 0".into());
                    row("Keys back from the console to the board", "Esc".into());
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "ITS-Board Emulator {} · not affiliated with the HAW Hamburg · the real board is the reference",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .small()
                    .weak(),
                );
            });
        self.show_help = open;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ---- take the latest snapshot
        let mut new_frame = false;
        let (
            state,
            firmware,
            led_duty,
            inputs,
            nucleo_duty,
            virtual_ns,
            core_mhz,
            speed,
            gdb,
            usb,
            dac1,
            pins,
            devices,
        ) = {
            let mut snap = self.emu.snapshot.lock().unwrap();
            if snap.display_generation != self.display_generation {
                self.display_generation = snap.display_generation;
                std::mem::swap(&mut snap.display, &mut self.display_frame);
                new_frame = true;
            }
            for (at_ns, bytes) in std::mem::take(&mut snap.serial) {
                self.console.received_bytes(&bytes, at_ns);
            }
            self.devices.take_output(std::mem::take(&mut snap.pico_output));
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
                self.show_pins.then_some(snap.pins),
                snap.devices.clone(),
            )
        };

        // shown alone and enlarged, the display's pixels stay sharp
        let options = if self.lcd_focus {
            egui::TextureOptions { magnification: egui::TextureFilter::Nearest, ..egui::TextureOptions::LINEAR }
        } else {
            egui::TextureOptions::LINEAR
        };
        if new_frame || options != self.display_options {
            self.display_options = options;
            let image = egui::ColorImage::from_rgba_unmultiplied([SCREEN_WIDTH, SCREEN_HEIGHT], &self.display_frame);
            match &mut self.display {
                Some(tex) => tex.set(image, options),
                None => self.display = Some(ctx.load_texture("display", image, options)),
            }
        }
        if let Some(text) = self.files.notice.take() {
            self.notify(&ctx, text, false);
        }

        // ---- keyboard shortcuts
        // keys without Ctrl only while no text field takes them
        let typing = ctx.text_edit_focused();
        let shortcut = |s: &KeyboardShortcut| {
            (s.modifiers != Modifiers::NONE || !typing) && ctx.input_mut(|i| i.consume_shortcut(s))
        };
        if shortcut(&OPEN) {
            self.files.open(&ctx, false);
        }
        if shortcut(&RESET) {
            self.emu.send(Command::Reset);
        }
        if shortcut(&SAVE) {
            self.save_display_dialog(&ctx);
        }
        if shortcut(&DISPLAY) {
            self.lcd_focus = !self.lcd_focus;
        }
        if shortcut(&PINS) {
            self.set_pins(!self.show_pins);
        }
        if shortcut(&ZOOM_RESET) {
            self.board_view = board::Zoom::default();
        }
        if shortcut(&SEE_THROUGH) {
            self.set_see_through(self.see_through.is_none());
        }
        if shortcut(&DEVICES) {
            self.show_devices = !self.show_devices;
        }
        if shortcut(&CONSOLE) {
            self.set_console(!self.settings.console);
        }
        if shortcut(&QUIT) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if shortcut(&RUN) {
            self.toggle_run(&state);
        }
        if shortcut(&HELP) {
            self.show_help = !self.show_help;
        }

        // ---- menu bar, toolbar
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &ctx, &state));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &ctx, &state, &usb, nucleo_duty, devices.len()));

        // ---- status bar
        egui::Panel::bottom("status").show(ui, |ui| {
            self.status_bar(ui, &state, &firmware, virtual_ns, core_mhz, speed, &gdb, &usb);
        });

        // ---- serial console, below the board or right of it
        if self.settings.console {
            self.place_console(&ctx);
            let panel = if self.console_right {
                egui::Panel::right("console-right").resizable(true).default_size(CONSOLE_W).min_size(240.0)
            } else {
                egui::Panel::bottom("console").resizable(true).default_size(CONSOLE_H).min_size(80.0)
            };
            let action = panel.show(ui, |ui| self.console.show(ui)).inner;
            if let Some(bytes) = action.send {
                self.emu.send(Command::Serial(bytes));
            }
            if action.hide {
                self.set_console(false);
            }
            if action.save {
                self.save_console_dialog(&ctx);
            }
            if action.options_changed {
                self.settings.line_ending = self.console.line_ending;
                self.settings.echo = self.console.echo;
                self.settings.timestamps = self.console.timestamps;
                self.settings.save();
            }
        }

        // ---- devices
        if self.show_devices {
            egui::Panel::right("devices").resizable(true).default_size(DEVICES_W).show(ui, |ui| {
                self.devices.show(ui, &devices, &mut self.settings, &self.emu);
            });
        }

        // ---- pin inspector
        if let Some(pins) = &pins {
            egui::Panel::right("pins")
                .resizable(true)
                .default_size(crate::pins::MIN_WIDTH + 8.0)
                .min_size(crate::pins::MIN_WIDTH)
                .show(ui, |ui| crate::pins::show(ui, pins));
        }

        // ---- the board
        let open_hint = (state == RunState::NoFirmware).then(|| {
            format!(
                "Click here or press {} to open an .axf file,\nor drop it onto the window",
                ctx.format_shortcut(&OPEN)
            )
        });
        let board_state = BoardState {
            led_duty: &led_duty,
            inputs,
            display: self.display.as_ref(),
            ints: &self.ints,
            latched: &self.latched,
            analog: &self.analog,
            dac1,
            nucleo_duty: &nucleo_duty,
            open_hint: open_hint.as_deref(),
        };
        let view = match self.see_through {
            Some(opacity) => board::View { nucleo_opacity: opacity, shield_opacity: opacity },
            None => board::View::default(),
        };
        // the bench the board lies on: dark in the dark theme, light grey in the light one
        let bench = if ctx.theme() == egui::Theme::Dark { Color32::from_gray(28) } else { Color32::from_gray(190) };
        let central =
            egui::CentralPanel::default().frame(egui::Frame::new().fill(bench).inner_margin(10.0)).show(ui, |ui| {
                if self.lcd_focus {
                    board::show_lcd(ui, &board_state)
                } else {
                    let tex = self.board.get_or_insert_with(|| board::Textures::load(ui.ctx()));
                    board::show(ui, tex, &board_state, &view, &mut self.board_view)
                }
            });
        let input = central.inner;
        if input.focus_toggled {
            self.lcd_focus = !self.lcd_focus;
        }
        // Esc closes the help, else leaves the display view; not when it only
        // left a text field or closes the file browser
        if !typing && !self.text_was_focused && !self.files.is_open() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.show_help {
                self.show_help = false;
            } else {
                self.lcd_focus = false;
            }
        }
        if input.open_requested {
            self.files.open(&ctx, false);
        }

        const KEYS: [Key; 8] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7];
        for (i, key) in KEYS.into_iter().enumerate() {
            if input.latch_toggled[i] {
                self.latched[i] = !self.latched[i];
            }
            let key_down = !typing && ctx.input(|inp| inp.key_down(key) && !inp.modifiers.command);
            let down = input.buttons[i] || self.latched[i] || key_down;
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

        self.help_window(&ctx);

        // ---- drag & drop of firmware files: a hint while a file is over the window
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-target")));
            let rect = central.response.rect.shrink(6.0);
            painter.rect_filled(rect, 8.0, Color32::from_black_alpha(170));
            painter.rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(2.0, ctx.global_style().visuals.selection.bg_fill),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to load the firmware",
                egui::FontId::proportional(28.0),
                Color32::WHITE,
            );
        }

        if let Some(shot) = &mut self.self_screenshot {
            shot.tick(&ctx);
        }

        // Drag & drop of firmware files, file dialog
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        let picked = self.files.show(&ctx);
        if let Some(path) = dropped.into_iter().next().or(picked) {
            self.load(path);
        }

        let title = match &firmware {
            Some(name) => format!("{name} — ITS-Board Emulator"),
            None => "ITS-Board Emulator".to_owned(),
        };
        self.text_was_focused = ctx.text_edit_focused();
        self.save_window(&ctx);
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

/// Switches egui to the chosen theme.
fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::Light => egui::ThemePreference::Light,
    });
}
