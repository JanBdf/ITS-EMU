//! The Devices panel: devices on the header pins (rotary encoder, DS18B20,
//! Raspberry Pi Pico) added, controlled and removed while the board runs.
//! A Pico has a terminal on its USB serial port and starts host programs
//! (such as the Python GUI of the square wave generator) on it.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Key, RichText};
use itsboard::pico::PicoSpec;
use itsboard::spec::{DeviceSpec, pin_name};

use crate::emu::{Command, DeviceState, DeviceView, EmuHandle};
use crate::settings::Settings;

const MAX_TERMINAL: usize = 32_000;
/// Runs a Python program written for the Pico on the emulated one's port.
const RUN_GUI_PY: &str = include_str!("../../../tools/pico/run_gui.py");

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Encoder,
    Ds18b20,
    Lm75,
    Iks01a3,
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
    encoder_pins: [(char, u32); 2],
    sensor_pin: (char, u32),
    lm75_addr: u8,
    net_forwards: String,
    pico_firmware: String,
    pico_wiring: Vec<(u8, (char, u32))>,
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
            encoder_pins: [('F', 0), ('F', 1)],
            sensor_pin: ('G', 2),
            lm75_addr: 0x48,
            net_forwards: "tcp:8080=80".to_owned(),
            pico_firmware: String::new(),
            pico_wiring: itsboard::pico::DEFAULT_WIRING.to_vec(),
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

    /// The panel, at most `max_height` high.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        devices: &[DeviceView],
        settings: &mut Settings,
        emu: &EmuHandle,
        max_height: f32,
    ) {
        self.poll_programs();
        crate::app::section_header(ui, "Devices", "on the board's pins, kept across firmware loads");
        egui::ScrollArea::vertical().id_salt("devices").auto_shrink([false, true]).max_height(max_height).show(
            ui,
            |ui| {
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
            },
        );
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
            DeviceSpec::Iks01a3 => "X-NUCLEO-IKS01A3  I2C1".to_owned(),
            DeviceSpec::Pico(spec) => format!("Pico  {}", file_name(&spec.firmware)),
            DeviceSpec::Network(_) => "Network  192.168.33.99".to_owned(),
        };
        egui::CollapsingHeader::new(RichText::new(title).strong()).id_salt(("device", d.id)).default_open(true).show(
            ui,
            |ui| {
                match (&d.spec, &d.state) {
                    (_, DeviceState::Failed(e)) => {
                        ui.label(RichText::new(e).color(crate::app::err_color(ui)));
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
                    (DeviceSpec::Iks01a3, DeviceState::Iks01a3(state)) => {
                        show_iks01a3(ui, d.id, *state, emu);
                    }
                    (DeviceSpec::Pico(spec), DeviceState::Starting) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("starting");
                        });
                        show_wiring(ui, spec);
                    }
                    (DeviceSpec::Pico(spec), DeviceState::Running { port, usb, notices }) => {
                        for notice in notices {
                            ui.label(RichText::new(format!("⚠ {notice}")).color(ui.visuals().warn_fg_color));
                        }
                        show_wiring(ui, spec);
                        show_pico_usb(ui, usb.as_ref());
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
                    Some(status) => {
                        ui.label(RichText::new(format!("{} ended: {status}", p.name)).color(crate::app::err_color(ui)))
                    }
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
            ui.label(RichText::new(e).color(crate::app::err_color(ui)));
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
        ui.horizontal_wrapped(|ui| {
            for (kind, label) in [
                (Kind::Pico, "Pico"),
                (Kind::Encoder, "Rotary encoder"),
                (Kind::Ds18b20, "DS18B20"),
                (Kind::Lm75, "LM75"),
                (Kind::Iks01a3, "IKS01A3"),
                (Kind::Network, "Network"),
            ] {
                if crate::app::toggle_button(ui, self.kind == kind, label).clicked() {
                    self.kind = kind;
                    self.add_error = None;
                }
            }
        });
        let text = match self.kind {
            Kind::Encoder => {
                for (n, name) in ["A", "B"].into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("Contact {name}:"));
                        pin_picker(ui, ("encoder", n), &mut self.encoder_pins[n]);
                    })
                    .response
                    .on_hover_text("Contacts A and B (to GND, pull-ups on the module); clockwise, A falls first");
                }
                format!("encoder@{},{}", pin_name(self.encoder_pins[0]), pin_name(self.encoder_pins[1]))
            }
            Kind::Ds18b20 => {
                ui.horizontal(|ui| {
                    ui.label("Data:");
                    pin_picker(ui, "ds18b20", &mut self.sensor_pin);
                })
                .response
                .on_hover_text("1-Wire data line (pull-up on the module); several sensors may share a pin");
                format!("ds18b20@{}", pin_name(self.sensor_pin))
            }
            Kind::Lm75 => {
                ui.horizontal(|ui| {
                    ui.label("Address:");
                    egui::ComboBox::from_id_salt("lm75-addr")
                        .selected_text(format!("0x{:02X}", self.lm75_addr))
                        .show_ui(ui, |ui| {
                            for a in 0x48..=0x4F_u8 {
                                ui.selectable_value(&mut self.lm75_addr, a, format!("0x{a:02X}"));
                            }
                        });
                })
                .response
                .on_hover_text("I2C address 0x48..0x4F (A2..A0) on the I2C header: I2C1, SCL PB8, SDA PB9");
                format!("lm75@0x{:02X}", self.lm75_addr)
            }
            Kind::Iks01a3 => {
                ui.label("X-NUCLEO-IKS01A3 on I2C1 (PB8/PB9): LSM6DSO, LIS2DW12, LIS2MDL, HTS221, LPS22HH, STTS751");
                "iks01a3".to_owned()
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
                        .hint_text("no file chosen (or paste the path of a .uf2 file)")
                        .desired_width(f32::INFINITY),
                );
                ui.label("Wires:").on_hover_text(
                    "Pico GPIOs and the board pins they are wired to (the square wave generator: \
                     GP0 → PF0 = IN0, GP5 → PF1 = IN1)",
                );
                let mut remove = None;
                for (i, (gp, pin)) in self.pico_wiring.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("pico-gp", i))
                            .width(56.0)
                            .selected_text(format!("GP{gp}"))
                            .show_ui(ui, |ui| {
                                for g in 0..=29_u8 {
                                    ui.selectable_value(gp, g, format!("GP{g}"));
                                }
                            });
                        ui.label("→");
                        pin_picker(ui, ("pico-pin", i), pin);
                        if ui.small_button("✖").on_hover_text("Remove this wire").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.pico_wiring.remove(i);
                }
                if ui.small_button("+ Wire").clicked() {
                    let gp = (0..=29).find(|g| self.pico_wiring.iter().all(|w| w.0 != *g)).unwrap_or(0);
                    self.pico_wiring.push((gp, ('F', 2)));
                }
                format!("pico={}@{}", self.pico_firmware.trim(), wiring_text(&self.pico_wiring))
            }
        };
        if ui.button("Connect").clicked() {
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
            ui.label(RichText::new(e).color(crate::app::err_color(ui)));
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
        // a hand turns one detent in about 25 ms, a quick spin takes 8 ms per detent
        for (label, detents, ns, hover) in [
            ("⟲ 10", -10, 2_000_000, "10 detents counter-clockwise (a quick spin)"),
            ("⟲ 1", -1, 6_000_000, "one detent counter-clockwise"),
            ("⟳ 1", 1, 6_000_000, "one detent clockwise"),
            ("⟳ 10", 10, 2_000_000, "10 detents clockwise (a quick spin)"),
        ] {
            if ui.button(label).on_hover_text(hover).clicked() {
                emu.send(Command::Turn(id, detents, ns));
            }
        }
        // the mouse wheel over the knob turns it one detent per notch
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
        let p = ui.painter();
        p.circle_filled(rect.center(), 12.0, ui.visuals().widgets.inactive.bg_fill);
        p.circle_stroke(rect.center(), 12.0, egui::Stroke::new(1.0, ui.visuals().widgets.inactive.fg_stroke.color));
        p.line_segment(
            [rect.center(), rect.center() + egui::vec2(0.0, -10.0)],
            egui::Stroke::new(2.0, ui.visuals().widgets.inactive.fg_stroke.color),
        );
        let resp = resp.on_hover_text("Scroll here to turn the knob (one detent per notch)");
        if resp.hovered() {
            let scroll: f32 = ui.input(|i| {
                i.raw
                    .events
                    .iter()
                    .map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 })
                    .sum()
            });
            if scroll != 0.0 {
                emu.send(Command::Turn(id, -scroll.signum() as i32, 6_000_000));
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

fn show_iks01a3(ui: &mut egui::Ui, id: u64, mut st: itsboard::iks01a3::Iks01a3State, emu: &EmuHandle) {
    let mut changed = false;
    egui::Grid::new(("iks01a3", id)).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        ui.label("Temperature:");
        changed |= ui
            .add(egui::Slider::new(&mut st.temp_c, -40.0..=85.0).suffix(" °C").step_by(0.1).max_decimals(1))
            .changed();
        ui.end_row();
        ui.label("Humidity:");
        changed |= ui
            .add(egui::Slider::new(&mut st.humidity_rh, 0.0..=100.0).suffix(" %RH").step_by(0.5).max_decimals(1))
            .changed();
        ui.end_row();
        ui.label("Pressure:");
        changed |= ui
            .add(egui::Slider::new(&mut st.pressure_hpa, 800.0..=1100.0).suffix(" hPa").step_by(0.25).max_decimals(2))
            .changed();
        ui.end_row();
        for (label, vals, range, unit) in [
            ("Accel:", &mut st.accel_mg, -2000.0..=2000.0, "mg"),
            ("Gyro:", &mut st.gyro_mdps, -250_000.0..=250_000.0, "mdps"),
            ("Mag:", &mut st.mag_mgauss, -2000.0..=2000.0, "mG"),
        ] {
            ui.label(label);
            ui.horizontal(|ui| {
                for (axis, v) in ["X", "Y", "Z"].into_iter().zip(vals.iter_mut()) {
                    ui.label(axis);
                    changed |= ui
                        .add(egui::DragValue::new(v).range(range.clone()).speed(10.0).max_decimals(0).suffix(unit))
                        .changed();
                }
            });
            ui.end_row();
        }
    });
    if changed {
        emu.send(Command::Iks01a3(id, st));
    }
}

fn show_wiring(ui: &mut egui::Ui, spec: &PicoSpec) {
    let wires: Vec<String> = spec.wiring.iter().map(|&(gp, pin)| format!("GP{gp} → {}", pin_name(pin))).collect();
    ui.label(wires.join(" · ")).on_hover_text(spec.firmware.display().to_string());
}

/// A board pin: port and number, and the name of its pad on the carrier.
fn pin_picker(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug + Copy, pin: &mut (char, u32)) {
    egui::ComboBox::from_id_salt((id, "port")).width(44.0).selected_text(format!("P{}", pin.0)).show_ui(ui, |ui| {
        for c in 'A'..='G' {
            ui.selectable_value(&mut pin.0, c, format!("P{c}"));
        }
    });
    egui::ComboBox::from_id_salt((id, "pin")).width(40.0).selected_text(pin.1.to_string()).show_ui(ui, |ui| {
        ui.set_min_width(110.0);
        for n in 0..16 {
            let label = match crate::board::pad(pin.0, n) {
                Some((name, _)) => format!("{n}  {name}"),
                None => n.to_string(),
            };
            ui.selectable_value(&mut pin.1, n, label);
        }
    });
    match crate::board::pad(pin.0, pin.1) {
        Some((name, _)) => ui.label(name),
        None => ui.label(RichText::new("Morpho header").weak()).on_hover_text("No pad on the carrier for this pin"),
    };
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
    if let DeviceSpec::Lm75(addr) = spec {
        if devices.iter().any(|d| d.spec == *spec) {
            return Err(format!("address 0x{addr:02X} is in use"));
        }
        if *addr == itsboard::iks01a3::ADDR_STTS751 && devices.iter().any(|d| d.spec == DeviceSpec::Iks01a3) {
            return Err("address 0x4A is in use by X-NUCLEO-IKS01A3 (STTS751)".to_owned());
        }
    }
    if *spec == DeviceSpec::Iks01a3 {
        if devices.iter().any(|d| d.spec == DeviceSpec::Iks01a3) {
            return Err("an X-NUCLEO-IKS01A3 shield is already connected".to_owned());
        }
        if devices.iter().any(|d| d.spec == DeviceSpec::Lm75(itsboard::iks01a3::ADDR_STTS751)) {
            return Err("address 0x4A (STTS751) is in use by LM75".to_owned());
        }
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

/// The Pico's USB device on the computer's USB (USB/IP), if exported.
fn show_pico_usb(ui: &mut egui::Ui, usb: Option<&(String, bool)>) {
    let Some((busid, attached)) = usb else { return };
    ui.horizontal(|ui| {
        ui.label("USB device:");
        if *attached {
            ui.label(RichText::new(format!("plugged in ({busid})")).color(crate::app::ok_color(ui))).on_hover_text(
                "Programs on this computer find it like a real Pico (e.g. /dev/ttyACM*); \
                 the pseudo terminal below is idle meanwhile",
            );
        } else {
            ui.label(RichText::new(format!("waiting for usbip attach ({busid})")).weak())
                .on_hover_text(format!("sudo usbip attach -r 127.0.0.1 -b {busid}"));
        }
    });
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
