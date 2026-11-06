//! The Devices panel: devices on the header pins (rotary encoder, DS18B20,
//! Raspberry Pi Pico) added, controlled and removed while the board runs.
//! A Pico has a terminal on its USB serial port and starts host programs
//! (such as the Python GUI of the square wave generator) on it.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Key, RichText};
use itsboard::pico::PicoSpec;
use itsboard::spec::{DeviceSpec, pin_name};

use crate::emu::{Command, DeviceState, DeviceView, EmuHandle};
use crate::settings::Settings;

const ERR_RED: Color32 = Color32::from_rgb(0xFF, 0x5B, 0x50);
const MAX_TERMINAL: usize = 32_000;
/// Runs a Python program written for the Pico on the emulated one's port.
const RUN_GUI_PY: &str = include_str!("../../../tools/pico/run_gui.py");

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Encoder,
    Ds18b20,
    Lm75,
    Pico,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineEnd {
    None,
    Lf,
    CrLf,
}

impl LineEnd {
    const ALL: [(Self, &'static str); 3] = [(Self::None, "no line end"), (Self::Lf, "LF"), (Self::CrLf, "CR LF")];

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }

    fn label(self) -> &'static str {
        Self::ALL.iter().find(|(e, _)| *e == self).map_or("", |(_, l)| l)
    }
}

/// A Pico's serial port as the panel shows it.
struct Terminal {
    text: String,
    input: String,
    line_end: LineEnd,
}

/// A host program on a Pico's serial port, and what it printed.
struct Program {
    name: String,
    child: Child,
    output: Arc<Mutex<String>>,
    exit: Option<std::process::ExitStatus>,
}

pub struct DevicesPanel {
    kind: Kind,
    encoder_pins: String,
    sensor_pin: String,
    lm75_addr: String,
    net_forwards: String,
    pico_firmware: String,
    pico_wiring: String,
    add_error: Option<String>,
    terminals: HashMap<u64, Terminal>,
    programs: HashMap<u64, Program>,
    program_error: Option<String>,
    /// Boot ROM given on the command line (not saved).
    bootrom_override: Option<PathBuf>,
}

impl DevicesPanel {
    pub fn new(bootrom_override: Option<PathBuf>) -> Self {
        Self {
            kind: Kind::Pico,
            encoder_pins: "PC6,PC7".to_owned(),
            sensor_pin: "PG2".to_owned(),
            lm75_addr: "0x48".to_owned(),
            net_forwards: "tcp:8080=80".to_owned(),
            pico_firmware: String::new(),
            pico_wiring: wiring_text(&itsboard::pico::DEFAULT_WIRING),
            add_error: None,
            terminals: HashMap::new(),
            programs: HashMap::new(),
            program_error: None,
            bootrom_override,
        }
    }

    /// Serial output of the Picos.
    pub fn take_output(&mut self, output: Vec<(u64, Vec<u8>)>) {
        for (id, data) in output {
            let t = self.terminal(id);
            t.text.push_str(&String::from_utf8_lossy(&data).replace("\r\n", "\n"));
            if t.text.len() > MAX_TERMINAL {
                let cut = t.text.len() - MAX_TERMINAL / 2;
                let cut = (cut..t.text.len()).find(|&i| t.text.is_char_boundary(i)).unwrap_or(0);
                t.text.drain(..cut);
            }
        }
    }

    fn terminal(&mut self, id: u64) -> &mut Terminal {
        self.terminals.entry(id).or_insert_with(|| Terminal {
            text: String::new(),
            input: String::new(),
            line_end: LineEnd::None,
        })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, devices: &[DeviceView], settings: &mut Settings, emu: &EmuHandle) {
        self.poll_programs();
        ui.heading("Devices");
        ui.label(RichText::new("On the header pins; they keep running when other firmware is loaded.").small().weak());
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if devices.is_empty() {
                ui.label(RichText::new("No devices connected").weak());
            }
            for d in devices {
                self.show_device(ui, d, devices, settings, emu);
            }
            ui.add_space(8.0);
            ui.separator();
            self.show_add(ui, devices, settings, emu);
            if devices.iter().any(|d| matches!(d.spec, DeviceSpec::Pico(_))) || self.kind == Kind::Pico {
                ui.separator();
                self.show_pico_options(ui, settings, emu);
            }
        });
    }

    fn show_device(
        &mut self,
        ui: &mut egui::Ui,
        d: &DeviceView,
        devices: &[DeviceView],
        settings: &mut Settings,
        emu: &EmuHandle,
    ) {
        let title = match &d.spec {
            DeviceSpec::Encoder(a, b) => format!("Rotary encoder  A {} · B {}", pin_name(*a), pin_name(*b)),
            DeviceSpec::Ds18b20(pin) => format!("DS18B20  {}", pin_name(*pin)),
            DeviceSpec::Lm75(addr) => format!("LM75  I2C1 0x{addr:02X}"),
            DeviceSpec::Pico(spec) => format!("Pico  {}", file_name(&spec.firmware)),
            DeviceSpec::Network(_) => "Network  192.168.33.99".to_owned(),
        };
        egui::CollapsingHeader::new(RichText::new(title).strong()).id_salt(("device", d.id)).default_open(true).show(
            ui,
            |ui| {
                match (&d.spec, &d.state) {
                    (_, DeviceState::Failed(e)) => {
                        ui.label(RichText::new(e).color(ERR_RED));
                    }
                    (DeviceSpec::Encoder(..), _) => show_encoder(ui, d.id, emu),
                    (DeviceSpec::Network(_), DeviceState::Network(bound)) => {
                        if bound.is_empty() {
                            ui.label("no ports forwarded");
                        }
                        for (from, port) in bound {
                            ui.label(format!("{from} → board port {port}"));
                        }
                    }
                    (DeviceSpec::Ds18b20(_) | DeviceSpec::Lm75(_), DeviceState::Sensor { celsius, rom }) => {
                        show_sensor(ui, d.id, *celsius, rom.as_ref(), emu);
                    }
                    (DeviceSpec::Pico(spec), DeviceState::Starting) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("starting");
                        });
                        show_wiring(ui, spec);
                    }
                    (DeviceSpec::Pico(spec), DeviceState::Running { port, notices }) => {
                        for notice in notices {
                            ui.label(RichText::new(format!("⚠ {notice}")).color(ui.visuals().warn_fg_color));
                        }
                        show_wiring(ui, spec);
                        self.show_pico(ui, d.id, port.as_deref(), settings, emu);
                    }
                    _ => {}
                }
                ui.horizontal(|ui| {
                    if ui.button("✖ Remove").on_hover_text("Disconnect the device from the board").clicked() {
                        emu.send(Command::RemoveDevice(d.id));
                        // the saved list holds the same devices in the same order
                        let nth = devices.iter().take_while(|o| o.id != d.id).filter(|o| o.spec == d.spec).count();
                        if let Some(i) = settings.devices.iter().enumerate().filter(|(_, s)| **s == d.spec).nth(nth) {
                            settings.devices.remove(i.0);
                        }
                        settings.save();
                        self.terminals.remove(&d.id);
                        if let Some(mut p) = self.programs.remove(&d.id) {
                            let _ = p.child.kill();
                        }
                    }
                    if matches!(d.spec, DeviceSpec::Pico(_))
                        && ui
                            .button("⟲ Power cycle")
                            .on_hover_text("Switch the Pico off and on again; reads the firmware file again")
                            .clicked()
                    {
                        emu.send(Command::RestartDevice(d.id));
                        self.terminals.remove(&d.id);
                    }
                });
            },
        );
    }

    fn show_pico(&mut self, ui: &mut egui::Ui, id: u64, port: Option<&Path>, settings: &mut Settings, emu: &EmuHandle) {
        match port {
            Some(port) => {
                ui.horizontal(|ui| {
                    ui.label("USB serial port:");
                    let path = port.display().to_string();
                    if ui.link(RichText::new(&path).monospace()).on_hover_text("Copy").clicked() {
                        ui.ctx().copy_text(path);
                    }
                });
            }
            None => {
                ui.label(RichText::new("No serial port for host programs on this system").weak());
            }
        }

        // a host program on the port
        if let Some(port) = port {
            self.show_program(ui, id, port, settings);
        }

        // the terminal
        let t = self.terminal(id);
        ui.label(RichText::new("Terminal").strong());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("pico-terminal", id)).max_height(140.0).stick_to_bottom(true).show(
                ui,
                |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(&t.text).monospace()).wrap());
                },
            );
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("line-end", id)).selected_text(t.line_end.label()).width(90.0).show_ui(
                ui,
                |ui| {
                    for (e, label) in LineEnd::ALL {
                        ui.selectable_value(&mut t.line_end, e, label);
                    }
                },
            );
            let edit =
                ui.add(egui::TextEdit::singleline(&mut t.input).hint_text("Enter sends").desired_width(f32::INFINITY));
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let mut data = std::mem::take(&mut t.input).into_bytes();
                data.extend_from_slice(t.line_end.bytes());
                if !data.is_empty() {
                    emu.send(Command::PicoSend(id, data));
                }
                edit.request_focus();
            }
        });
    }

    fn show_program(&mut self, ui: &mut egui::Ui, id: u64, port: &Path, settings: &mut Settings) {
        if let Some(p) = self.programs.get_mut(&id) {
            let mut close = false;
            ui.horizontal(|ui| {
                match p.exit {
                    None => ui.label(format!("{} runs", p.name)),
                    Some(status) if status.success() => ui.label(format!("{} ended", p.name)),
                    Some(status) => ui.label(RichText::new(format!("{} ended: {status}", p.name)).color(ERR_RED)),
                };
                if p.exit.is_none() {
                    if ui.button("■ Stop").clicked() {
                        let _ = p.child.kill();
                    }
                } else if ui.small_button("✖").on_hover_text("Close").clicked() {
                    close = true;
                }
            });
            if close {
                self.programs.remove(&id);
            }
            if let Some(p) = self.programs.get(&id) {
                let output = p.output.lock().unwrap();
                if !output.is_empty() {
                    egui::ScrollArea::vertical()
                        .id_salt(("program-output", id))
                        .max_height(80.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(output.as_str()).monospace().small()).wrap());
                        });
                }
            }
            return;
        }
        ui.horizontal(|ui| {
            let last = settings.python_program.clone();
            let hover = "Start a Python program written for the Pico (pyserial) on this port, unchanged: \
                         its list of serial ports contains this one as a Pico";
            if ui.button("▶ Python program…").on_hover_text(hover).clicked() {
                let mut dialog = rfd::FileDialog::new().add_filter("Python", &["py"]).set_title("Python program");
                if let Some(dir) = last.as_deref().and_then(Path::parent) {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(program) = dialog.pick_file() {
                    self.start_program(id, port, &program, settings);
                }
            }
            if let Some(program) = &last
                && ui
                    .button(format!("▶ {}", file_name(program)))
                    .on_hover_text(format!("Start {} again", program.display()))
                    .clicked()
            {
                self.start_program(id, port, &program.clone(), settings);
            }
        });
        if let Some(e) = &self.program_error {
            ui.label(RichText::new(e).color(ERR_RED));
        }
    }

    fn start_program(&mut self, id: u64, port: &Path, program: &Path, settings: &mut Settings) {
        settings.python_program = Some(program.to_owned());
        settings.save();
        match spawn_program(&settings.python, port, program) {
            Ok((child, output)) => {
                self.program_error = None;
                self.programs.insert(id, Program { name: file_name(program), child, output, exit: None });
            }
            Err(e) => self.program_error = Some(format!("cannot start {}: {e}", settings.python)),
        }
    }

    fn poll_programs(&mut self) {
        for p in self.programs.values_mut() {
            if p.exit.is_none() {
                p.exit = p.child.try_wait().ok().flatten();
            }
        }
    }

    fn show_add(&mut self, ui: &mut egui::Ui, devices: &[DeviceView], settings: &mut Settings, emu: &EmuHandle) {
        ui.label(RichText::new("Connect a device").strong());
        ui.horizontal(|ui| {
            for (kind, label) in [
                (Kind::Pico, "Pico"),
                (Kind::Encoder, "Rotary encoder"),
                (Kind::Ds18b20, "DS18B20"),
                (Kind::Lm75, "LM75"),
                (Kind::Network, "Network"),
            ] {
                if ui.selectable_label(self.kind == kind, label).clicked() {
                    self.kind = kind;
                    self.add_error = None;
                }
            }
        });
        let text = match self.kind {
            Kind::Encoder => {
                ui.horizontal(|ui| {
                    ui.label("Pins A, B:");
                    ui.text_edit_singleline(&mut self.encoder_pins);
                })
                .response
                .on_hover_text("Contacts A and B (to GND, pull-ups on the module); clockwise, A falls first");
                format!("encoder@{}", self.encoder_pins)
            }
            Kind::Ds18b20 => {
                ui.horizontal(|ui| {
                    ui.label("Pin:");
                    ui.text_edit_singleline(&mut self.sensor_pin);
                })
                .response
                .on_hover_text("1-Wire data line (pull-up on the module); several sensors may share a pin");
                format!("ds18b20@{}", self.sensor_pin)
            }
            Kind::Lm75 => {
                ui.horizontal(|ui| {
                    ui.label("Address:");
                    ui.text_edit_singleline(&mut self.lm75_addr);
                })
                .response
                .on_hover_text("I2C address 0x48..0x4F (A2..A0) on the I2C header: I2C1, SCL PB8, SDA PB9");
                format!("lm75@{}", self.lm75_addr)
            }
            Kind::Network => {
                ui.label("Ethernet: gateway 192.168.33.1, DHCP gives the board 192.168.33.99");
                ui.horizontal(|ui| {
                    ui.label("Forwarded ports:");
                    ui.text_edit_singleline(&mut self.net_forwards);
                })
                .response
                .on_hover_text(
                    "Ports of this computer (localhost) and the board's ports they reach: \
                     tcp:8080=80,udp:5000 (empty: none)",
                );
                format!("net@{}", self.net_forwards)
            }
            Kind::Pico => {
                ui.horizontal(|ui| {
                    ui.label("Firmware:");
                    if ui.button("Choose…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("Pico firmware", &["uf2"])
                            .set_title("Pico firmware")
                            .pick_file()
                    {
                        self.pico_firmware = path.display().to_string();
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.pico_firmware)
                        .hint_text("FIRMWARE.uf2")
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    ui.label("Wiring:");
                    ui.add(egui::TextEdit::singleline(&mut self.pico_wiring).desired_width(f32::INFINITY));
                })
                .response
                .on_hover_text(
                    "Pico GPIOs and the board pins they are wired to: GP0=PF0,GP5=PF1 \
                     (the square wave generator's A0, A5 on S0, S1)",
                );
                format!("pico={}@{}", self.pico_firmware.trim(), self.pico_wiring)
            }
        };
        if ui.button("➕ Connect").clicked() {
            match DeviceSpec::parse(&text).and_then(|spec| check_pins(&spec, devices).map(|()| spec)) {
                Ok(spec) => {
                    self.add_error = None;
                    emu.send(Command::AddDevice(spec.clone()));
                    settings.devices.push(spec);
                    settings.save();
                }
                Err(e) => self.add_error = Some(e),
            }
        }
        if let Some(e) = &self.add_error {
            ui.label(RichText::new(e).color(ERR_RED));
        }
    }

    fn show_pico_options(&mut self, ui: &mut egui::Ui, settings: &mut Settings, emu: &EmuHandle) {
        ui.label(RichText::new("Pico").strong());
        ui.horizontal_wrapped(|ui| {
            ui.label("Boot ROM:");
            let rom = self.bootrom_override.as_ref().or(settings.pico_bootrom.as_ref());
            match rom {
                Some(path) => ui.label(file_name(path)).on_hover_text(path.display().to_string()),
                None => ui.label("download (cached)").on_hover_text(format!(
                    "The RP2040's boot ROM, downloaded once from the Raspberry Pi GitHub release into {}",
                    rp2040::bootrom::cache_path().display()
                )),
            };
            if self.bootrom_override.is_some() {
                ui.label(RichText::new("(--pico-bootrom)").weak());
                return;
            }
            if ui.button("Choose…").on_hover_text("16 KiB image, or the ELF file of pico-bootrom").clicked()
                && let Some(path) = rfd::FileDialog::new().set_title("RP2040 boot ROM").pick_file()
            {
                settings.pico_bootrom = Some(path.clone());
                settings.save();
                emu.send(Command::SetBootrom(Some(path)));
            }
            if settings.pico_bootrom.is_some() && ui.button("Download").clicked() {
                settings.pico_bootrom = None;
                settings.save();
                emu.send(Command::SetBootrom(None));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Python:");
            if ui
                .add(egui::TextEdit::singleline(&mut settings.python).desired_width(120.0))
                .on_hover_text("Interpreter for host programs (needs pyserial: python3 -m pip install pyserial)")
                .lost_focus()
            {
                settings.save();
            }
        });
    }
}

impl Drop for DevicesPanel {
    fn drop(&mut self) {
        for p in self.programs.values_mut() {
            let _ = p.child.kill();
        }
    }
}

fn show_encoder(ui: &mut egui::Ui, id: u64, emu: &EmuHandle) {
    ui.horizontal(|ui| {
        ui.label("Turn:");
        for (label, detents, hover) in [
            ("⟲ 10", -10, "10 detents counter-clockwise"),
            ("⟲ 1", -1, "one detent counter-clockwise"),
            ("⟳ 1", 1, "one detent clockwise"),
            ("⟳ 10", 10, "10 detents clockwise"),
        ] {
            if ui.button(label).on_hover_text(hover).clicked() {
                emu.send(Command::Turn(id, detents));
            }
        }
    });
}

fn show_sensor(ui: &mut egui::Ui, id: u64, celsius: f32, rom: Option<&[u8; 8]>, emu: &EmuHandle) {
    ui.horizontal(|ui| {
        ui.label("Temperature:");
        let mut c = celsius;
        let slider = egui::Slider::new(&mut c, -55.0..=125.0).suffix(" °C").step_by(0.0625).max_decimals(2);
        if ui.add(slider).changed() {
            emu.send(Command::Temperature(id, c));
        }
    });
    if let Some(rom) = rom {
        let hex: Vec<String> = rom.iter().map(|b| format!("{b:02X}")).collect();
        ui.label(RichText::new(format!("ROM {}", hex.join(" "))).monospace().small().weak())
            .on_hover_text("Family code, serial number, CRC (as Read ROM sends them)");
    }
}

fn show_wiring(ui: &mut egui::Ui, spec: &PicoSpec) {
    let wires: Vec<String> = spec.wiring.iter().map(|&(gp, pin)| format!("GP{gp} → {}", pin_name(pin))).collect();
    ui.label(wires.join(" · ")).on_hover_text(spec.firmware.display().to_string());
}

/// `GP0=PF0,GP5=PF1`
fn wiring_text(wiring: &[(u8, (char, u32))]) -> String {
    wiring.iter().map(|&(gp, pin)| format!("GP{gp}={}", pin_name(pin))).collect::<Vec<_>>().join(",")
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// A new device must not drive a pin another one drives (DS18B20 sensors
/// share their 1-Wire line), nor use an I2C address another one uses.
fn check_pins(spec: &DeviceSpec, devices: &[DeviceView]) -> Result<(), String> {
    if let DeviceSpec::Lm75(addr) = spec
        && devices.iter().any(|d| d.spec == *spec)
    {
        return Err(format!("address 0x{addr:02X} is in use"));
    }
    if matches!(spec, DeviceSpec::Network(_)) && devices.iter().any(|d| matches!(d.spec, DeviceSpec::Network(_))) {
        return Err("the board has one Ethernet jack (remove the network first)".to_owned());
    }
    for pin in spec.board_pins() {
        let shared_line =
            |other: &DeviceSpec| matches!((spec, other), (DeviceSpec::Ds18b20(_), DeviceSpec::Ds18b20(_)));
        if let Some(d) = devices.iter().find(|d| d.spec.board_pins().contains(&pin) && !shared_line(&d.spec)) {
            return Err(format!("{} is in use ({})", pin_name(pin), d.spec));
        }
    }
    Ok(())
}

/// Starts `program` with `python` on the Pico's serial port through
/// `run_gui.py`; its output is collected.
fn spawn_program(python: &str, port: &Path, program: &Path) -> std::io::Result<(Child, Arc<Mutex<String>>)> {
    let launcher = std::env::temp_dir().join("its-emu-run_gui.py");
    std::fs::write(&launcher, RUN_GUI_PY)?;
    let mut child = std::process::Command::new(python)
        .arg(&launcher)
        .arg(port)
        .arg(program)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = Arc::new(Mutex::new(String::new()));
    for mut pipe in [
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        child.stderr.take().map(|p| Box::new(p) as _),
    ]
    .into_iter()
    .flatten()
    {
        let output = output.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n @ 1..) = pipe.read(&mut buf) {
                let mut out = output.lock().unwrap();
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                let len = out.len();
                if len > MAX_TERMINAL {
                    let cut = (len - MAX_TERMINAL / 2..len).find(|&i| out.is_char_boundary(i)).unwrap_or(0);
                    out.drain(..cut);
                }
            }
        });
    }
    Ok((child, output))
}
