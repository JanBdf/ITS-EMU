//! The emulator runs on its own thread: `Simulator` is not `Send` (Unicorn,
//! shared peripheral state), so it is created and used only there. The UI
//! sends [`Command`]s and reads the latest [`Snapshot`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use emu_core::gdb::{Execution, GdbServer};
use emu_core::{ElfImage, StopReason};
use itsboard::sim::DEFAULT_FONTS;
use itsboard::{SCREEN_HEIGHT, SCREEN_WIDTH, Simulator};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Speed {
    /// Virtual time runs at wall-clock speed times this factor.
    Factor(f64),
    /// As fast as possible.
    Max,
}

#[derive(Debug)]
pub enum Command {
    Load(PathBuf),
    Reset,
    SetPaused(bool),
    SetSpeed(Speed),
    Button(u32, bool),
    UserButton(bool),
    Touch(Option<(f32, f32)>),
    /// Level of INT0–5 / AUX0–5 (PG0–5): `None` = open.
    Int(u32, Option<bool>),
    /// Voltage on a pad of the Analog header (`itsboard::ANALOG_PADS`): `None` = open.
    Analog(usize, Option<f32>),
    Serial(Vec<u8>),
    /// Show the frame buffer's ideal colours instead of the measured look of
    /// the real panel (tone response, grey black level).
    IdealColors(bool),
    /// Plug the board's ST-LINK in (USB/IP) or unplug it.
    SetUsb(bool),
    /// A USB request arrived.
    Wake,
    Quit,
}

/// The board's ST-LINK on USB/IP.
#[derive(Debug, Clone)]
pub struct UsbConfig {
    /// USB/IP server address.
    pub addr: String,
    /// Plug it in at start.
    pub enabled: bool,
    /// Attach it to this computer (`usbip attach`, through the packaged
    /// polkit helper or sudo) whenever it is enabled.
    pub attach: bool,
}

/// State of the ST-LINK as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum UsbStatus {
    Off,
    /// The USB/IP server could not start.
    Unavailable(String),
    /// Exported, but no host has attached it (yet): the command that does,
    /// and why the automatic attach failed, if it did.
    Unplugged {
        addr: String,
        cmd: String,
        error: Option<String>,
    },
    /// Attached; `debugging`: a debugger has it in debug mode.
    PluggedIn {
        addr: String,
        debugging: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    NoFirmware,
    Running,
    Paused,
    /// A debugger (GDB) halted the core.
    Halted,
    /// The core stopped (breakpoint, lockup); reset to continue.
    Stopped(String),
    Error(String),
}

/// What the UI shows. Updated by the emulator thread.
pub struct Snapshot {
    pub firmware: Option<String>,
    pub state: RunState,
    pub display: Vec<u8>,
    pub display_generation: u64,
    pub leds: u16,
    /// Time-averaged brightness of OUT0..15 since the previous snapshot.
    pub led_duty: [f32; 16],
    pub inputs: u8,
    pub nucleo_leds: [bool; 3],
    pub nucleo_duty: [f32; 3],
    pub backlight: f32,
    pub virtual_ns: u64,
    pub core_mhz: u32,
    /// Virtual seconds per wall-clock second over the last measurement.
    pub speed: f64,
    /// Serial output not yet taken by the UI.
    pub serial: Vec<u8>,
    /// GDB server address, or why it is not available.
    pub gdb: Result<String, String>,
    /// A debugger is connected.
    pub gdb_attached: bool,
    pub usb: UsbStatus,
    /// Output voltage of DAC1 while it is enabled.
    pub dac1: Option<f32>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            firmware: None,
            state: RunState::NoFirmware,
            display: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
            display_generation: 0,
            leds: 0,
            led_duty: [0.0; 16],
            inputs: 0xFF,
            nucleo_leds: [false; 3],
            nucleo_duty: [0.0; 3],
            backlight: 0.0,
            virtual_ns: 0,
            core_mhz: 0,
            speed: 0.0,
            serial: Vec::new(),
            gdb: Err("off".into()),
            gdb_attached: false,
            usb: UsbStatus::Off,
            dac1: None,
        }
    }
}

pub type SharedSnapshot = Arc<Mutex<Snapshot>>;

pub struct EmuHandle {
    pub commands: Sender<Command>,
    pub snapshot: SharedSnapshot,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EmuHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

impl Drop for EmuHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts the emulator thread. `repaint` is called whenever the snapshot
/// changed. With `gdb_addr`, a GDB server listens there (except while
/// pyOCD uses the ST-LINK: its own GDB server takes the port).
pub fn spawn(repaint: impl Fn() + Send + 'static, gdb_addr: Option<String>, usb: UsbConfig) -> EmuHandle {
    let (tx, rx) = channel();
    let snapshot = SharedSnapshot::default();
    let shared = snapshot.clone();
    // USB requests wake the thread while it waits for commands (one pending wake is enough).
    let pending = Arc::new(AtomicBool::new(false));
    let wake_tx = tx.clone();
    let wake_pending = pending.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if !wake_pending.swap(true, Ordering::AcqRel) {
            let _ = wake_tx.send(Command::Wake);
        }
    });
    let thread = std::thread::Builder::new()
        .name("emulator".into())
        .spawn(move || Worker::new(rx, shared, Box::new(repaint), gdb_addr, usb, wake, pending).run())
        .expect("spawn emulator thread");
    EmuHandle { commands: tx, snapshot, thread: Some(thread) }
}

/// Virtual time executed between two looks at the command queue / clock.
const SLICE: Duration = Duration::from_millis(4);
/// Minimum interval between snapshot updates (UI refresh).
const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);

struct Worker {
    commands: Receiver<Command>,
    snapshot: SharedSnapshot,
    repaint: Box<dyn Fn() + Send>,
    sim: Option<Simulator>,
    firmware: Option<PathBuf>,
    /// A debugger has written flash since the last load (`firmware` is stale).
    uploaded: bool,
    state: RunState,
    speed: Speed,
    /// Pacing reference: (wall instant, virtual ns) at the last speed change/resume.
    anchor: (Instant, u64),
    last_publish: Instant,
    rate_sample: (Instant, u64),
    measured_speed: f64,
    published_generation: u64,
    /// The next display frame, rendered outside the snapshot lock and then
    /// swapped in.
    display: Vec<u8>,
    ideal_colors: bool,
    /// External inputs, re-applied when new firmware is loaded.
    buttons: [bool; 8],
    ints: [Option<bool>; 6],
    analog: [Option<f32>; 8],
    gdb: Option<GdbServer>,
    gdb_status: Result<String, String>,
    /// Address the GDB server should listen on, if any.
    gdb_addr: Option<String>,
    /// Next attempt to (re)open the GDB server's port.
    gdb_retry_at: Instant,
    /// How the debugger wants the core to run (`Free` without debugger).
    execution: Execution,
    usb: Option<stlink::UsbProbe>,
    usb_config: UsbConfig,
    usb_server_error: Option<String>,
    /// Outcome of the running `usbip attach`, and the last failure.
    attach_result: Option<Receiver<Result<(), String>>>,
    attach_error: Option<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Cleared when a [`Command::Wake`] is taken.
    wake_pending: Arc<AtomicBool>,
    /// The core was halted by the debug probe at the last look.
    core_halted: bool,
    /// Flash write count known to the worker: the probe's flash algorithm
    /// programs while the machine runs.
    seen_flash_writes: u64,
    /// The probe has held the core halted since this instant (time passes for the peripherals).
    halted_since: Option<Instant>,
}

impl Worker {
    fn new(
        commands: Receiver<Command>,
        snapshot: SharedSnapshot,
        repaint: Box<dyn Fn() + Send>,
        gdb_addr: Option<String>,
        usb_config: UsbConfig,
        wake: Arc<dyn Fn() + Send + Sync>,
        wake_pending: Arc<AtomicBool>,
    ) -> Self {
        let now = Instant::now();
        let mut w = Self {
            commands,
            snapshot,
            repaint,
            // Like a board with empty flash: a debugger can attach and load a program.
            sim: blank_board(),
            firmware: None,
            uploaded: false,
            state: RunState::NoFirmware,
            speed: Speed::Factor(1.0),
            anchor: (now, 0),
            last_publish: now,
            rate_sample: (now, 0),
            measured_speed: 0.0,
            published_generation: u64::MAX,
            display: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
            ideal_colors: false,
            buttons: [false; 8],
            ints: [None; 6],
            analog: [None; 8],
            gdb: None,
            gdb_status: Err("off".to_owned()),
            gdb_addr,
            gdb_retry_at: now,
            execution: Execution::Free,
            usb: None,
            usb_config: usb_config.clone(),
            usb_server_error: None,
            attach_result: None,
            attach_error: None,
            wake,
            wake_pending,
            core_halted: false,
            seen_flash_writes: 0,
            halted_since: None,
        };
        w.manage_gdb();
        if usb_config.enabled {
            w.set_usb(true);
        }
        w.adopt_sim();
        w
    }

    /// Plugs the ST-LINK in (starts the USB/IP server, attaches it) or unplugs it.
    fn set_usb(&mut self, on: bool) {
        if !on {
            if self.usb.take().is_some() {
                log::info!("ST-LINK unplugged");
            }
            self.attach_result = None;
            self.attach_error = None;
            self.usb_server_error = None;
            self.core_halted = false;
            if let Some(sim) = &mut self.sim {
                sim.machine.system_mut().bkpt_stops = true;
                sim.board.mcu().usart3.borrow_mut().mirror = None;
            }
            return;
        }
        if self.usb.is_some() {
            return;
        }
        match stlink::UsbProbe::start(&self.usb_config.addr, None, self.wake.clone()) {
            Ok(probe) => {
                log::info!("ST-LINK on USB/IP {}; attach with {}", probe.local_addr(), probe.attach_command());
                if self.usb_config.attach {
                    self.attach_result = Some(stlink::spawn_attach(probe.local_addr()));
                }
                self.usb = Some(probe);
                self.usb_server_error = None;
                self.adopt_sim();
            }
            Err(e) => {
                log::warn!("cannot start the USB/IP server on {}: {e}", self.usb_config.addr);
                self.usb_server_error = Some(format!("{}: {e}", self.usb_config.addr));
            }
        }
    }

    fn usb_status(&self) -> UsbStatus {
        match &self.usb {
            None => self.usb_server_error.clone().map_or(UsbStatus::Off, UsbStatus::Unavailable),
            Some(p) if p.is_attached() => {
                UsbStatus::PluggedIn { addr: p.local_addr().to_string(), debugging: p.debug_active() }
            }
            Some(p) => UsbStatus::Unplugged {
                addr: p.local_addr().to_string(),
                cmd: p.attach_command(),
                error: self.attach_error.clone(),
            },
        }
    }

    /// Opens the GDB server's port, and leaves it to pyOCD while pyOCD has
    /// the ST-LINK in debug mode (`pyocd gdbserver` listens on the same port).
    fn manage_gdb(&mut self) {
        let Some(addr) = self.gdb_addr.clone() else { return };
        let probe_busy = self.usb.as_ref().is_some_and(stlink::UsbProbe::debug_active);
        match &self.gdb {
            Some(server) if probe_busy && !server.has_controller() => {
                log::info!("GDB server closed: pyOCD uses the ST-LINK");
                self.gdb = None;
                self.gdb_status = Err("port left to pyOCD (ST-LINK in use)".to_owned());
                self.execution = Execution::Free;
            }
            None if !probe_busy && Instant::now() >= self.gdb_retry_at => match GdbServer::bind(addr.as_str()) {
                Ok(server) => {
                    let addr = server.local_addr().map_or(addr, |a| a.to_string());
                    log::info!("GDB server on {addr}");
                    self.gdb = Some(server);
                    self.gdb_status = Ok(addr);
                }
                Err(e) => {
                    // e.g. pyOCD's GDB server still holds the port: try again later
                    let msg = format!("{addr}: {e}");
                    if self.gdb_status.as_ref().err() != Some(&msg) {
                        log::warn!("cannot start GDB server on {msg}");
                    }
                    self.gdb_status = Err(msg);
                    self.gdb_retry_at = Instant::now() + Duration::from_secs(2);
                }
            },
            _ => {}
        }
    }

    /// Connects a new board to the USB probe.
    fn adopt_sim(&mut self) {
        self.seen_flash_writes = self.sim.as_ref().map_or(0, |s| s.machine.flash_writes());
        let Some(usb) = &mut self.usb else { return };
        if let Some(sim) = &mut self.sim {
            // The probe may leave the core on a BKPT: a HardFault on the chip.
            sim.machine.system_mut().bkpt_stops = false;
            usb.set_serial(Some(sim.board.mcu().usart3.clone()));
        } else {
            usb.set_serial(None);
        }
    }

    fn virtual_ns(&self) -> u64 {
        self.sim.as_ref().map_or(0, Simulator::time_ns)
    }

    fn reanchor(&mut self) {
        self.anchor = (Instant::now(), self.virtual_ns());
        self.rate_sample = self.anchor;
    }

    fn run(mut self) {
        loop {
            // While not running, block on the command queue (shortly while a
            // debugger may send packets).
            let idle_wait = if self.gdb.is_some() { Duration::from_millis(5) } else { Duration::from_millis(50) };
            // USB requests wake the thread (Command::Wake).
            let wait = if self.state == RunState::Running { Duration::ZERO } else { idle_wait };
            match self.commands.recv_timeout(wait) {
                Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(cmd) => {
                    self.handle(cmd);
                    while let Ok(cmd) = self.commands.try_recv() {
                        if matches!(cmd, Command::Quit) {
                            return;
                        }
                        self.handle(cmd);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.poll_gdb();
            self.poll_usb();
            self.manage_gdb();
            self.advance_halted();
            if self.state == RunState::Running {
                self.step();
            }
            self.publish(false);
        }
    }

    fn handle(&mut self, cmd: Command) {
        if matches!(cmd, Command::Wake) {
            // handled by poll_usb below
            self.wake_pending.store(false, Ordering::Release);
            return;
        }
        match cmd {
            Command::Load(path) => self.load(path),
            Command::Reset => {
                if let RunState::Error(_) = self.state
                    && !self.uploaded
                    && let Some(path) = self.firmware.clone()
                {
                    // Try loading the file again.
                    self.load(path);
                } else if let Some(sim) = &mut self.sim {
                    sim.reset();
                    // Nothing to run on a blank board.
                    if self.firmware.is_some() || self.uploaded {
                        self.state = RunState::Running;
                        self.reanchor();
                    }
                } else if let Some(path) = self.firmware.clone() {
                    self.load(path);
                }
            }
            Command::SetPaused(paused) => {
                if paused && self.state == RunState::Running {
                    self.state = RunState::Paused;
                } else if !paused && self.state == RunState::Paused {
                    self.state = RunState::Running;
                    self.reanchor();
                }
            }
            Command::SetSpeed(speed) => {
                self.speed = speed;
                self.reanchor();
            }
            Command::Button(n, pressed) => {
                self.buttons[n as usize] = pressed;
                self.with_board(|b| b.set_button(n, pressed));
            }
            Command::UserButton(pressed) => self.with_board(|b| b.set_user_button(pressed)),
            Command::Touch(pos) => self.with_board(|b| b.set_touch(pos)),
            Command::Int(n, level) => {
                self.ints[n as usize] = level;
                self.with_board(|b| b.set_int(n, level));
            }
            Command::Analog(n, volts) => {
                self.analog[n] = volts;
                self.with_board(|b| b.set_analog(n, volts));
            }
            Command::Serial(data) => self.with_board(|b| b.mcu().usart3.borrow_mut().send(&data)),
            Command::IdealColors(on) => {
                self.ideal_colors = on;
                self.published_generation = u64::MAX;
            }
            Command::SetUsb(on) => self.set_usb(on),
            Command::Wake | Command::Quit => {}
        }
        self.publish(true);
    }

    /// Lets the debugger talk to the target and follows its run/halt requests.
    fn poll_gdb(&mut self) {
        let (Some(gdb), Some(sim)) = (&mut self.gdb, &mut self.sim) else { return };
        let flash_writes = sim.machine.flash_writes();
        let execution = gdb.poll(&mut sim.machine);
        let changed = execution != self.execution;
        self.execution = execution;
        let loaded = sim.machine.take_gdb_loaded();
        let gdb_wrote = sim.machine.flash_writes() != flash_writes;
        if gdb_wrote {
            self.seen_flash_writes = sim.machine.flash_writes();
        }
        if loaded.is_some() || gdb_wrote {
            match loaded {
                // `monitor load FILE`: as if the file had been opened.
                Some((path, image)) => {
                    sim.image = image;
                    self.firmware = Some(path);
                    self.uploaded = false;
                }
                // Written by GDB's `load`: the symbols of the old file are stale.
                None => {
                    sim.image = ElfImage { entry: 0, segments: Vec::new(), symbols: Vec::new() };
                    self.uploaded = true;
                }
            }
            if matches!(self.state, RunState::NoFirmware | RunState::Error(_)) {
                self.state = RunState::Halted;
            }
        }
        match execution {
            // Also after a reset from the UI: the debugger keeps the core halted.
            Execution::Halted if self.state == RunState::Running => self.state = RunState::Halted,
            Execution::Halted => return,
            Execution::Running | Execution::Free if changed => {
                // Continuing or detaching resumes the core, as on the real board.
                if matches!(self.state, RunState::Halted | RunState::Paused | RunState::Stopped(_))
                    || (self.state == RunState::NoFirmware && execution == Execution::Running)
                {
                    self.state = RunState::Running;
                    self.reanchor();
                }
            }
            Execution::Running | Execution::Free => return,
        }
        self.publish(true);
    }

    /// Lets the USB host talk to the probe and follows the core's debug state.
    fn poll_usb(&mut self) {
        if let Some(rx) = &self.attach_result
            && let Ok(result) = rx.try_recv()
        {
            self.attach_error = result.err();
            self.attach_result = None;
            self.publish(true);
        }
        let (Some(usb), Some(sim)) = (&mut self.usb, &mut self.sim) else { return };
        let progress = usb.poll(&mut sim.machine);
        let mut changed = progress;
        if sim.machine.flash_writes() != self.seen_flash_writes {
            // Programmed through the probe: the symbols of the old file are stale.
            self.seen_flash_writes = sim.machine.flash_writes();
            sim.image = ElfImage { entry: 0, segments: Vec::new(), symbols: Vec::new() };
            self.uploaded = true;
            changed = true;
        }
        let halted = sim.machine.is_halted();
        let locked_up = sim.machine.is_locked_up();
        if halted != self.core_halted {
            self.core_halted = halted;
            changed = true;
            if halted && matches!(self.state, RunState::Running | RunState::Stopped(_) | RunState::NoFirmware) {
                self.state = RunState::Halted;
            } else if !halted && self.state == RunState::Halted && self.execution != Execution::Halted {
                // resumed (or reset without halt) by the probe: runs like the board
                self.state = RunState::Running;
                self.reanchor();
            }
        }
        if let RunState::Stopped(_) = self.state
            && !locked_up
            && !halted
        {
            // the probe reset a locked-up core
            self.state = RunState::Running;
            self.reanchor();
            changed = true;
        }
        if changed {
            self.publish(false);
        }
    }

    /// While the probe halts the core, the peripherals and the flash
    /// interface run on in real time (as on the board).
    fn advance_halted(&mut self) {
        let now = Instant::now();
        let since = self.halted_since.replace(now);
        let probe_halt = self.usb.is_some() && self.execution != Execution::Halted;
        match &mut self.sim {
            Some(sim) if probe_halt && sim.machine.is_halted() => {
                if let Some(t) = since {
                    let cycles = sim.machine.clock().ns_to_cycles((now - t).as_nanos() as u64);
                    sim.machine.advance_halted(cycles);
                }
            }
            _ => self.halted_since = None,
        }
    }

    fn with_board(&mut self, f: impl FnOnce(&itsboard::ItsBoard)) {
        if let Some(sim) = &self.sim {
            f(&sim.board);
        }
    }

    /// Applies the inputs the user has set to a new board.
    fn restore_inputs(&self) {
        let Some(sim) = &self.sim else { return };
        for (n, &pressed) in self.buttons.iter().enumerate() {
            sim.board.set_button(n as u32, pressed);
        }
        for (n, &level) in self.ints.iter().enumerate() {
            sim.board.set_int(n as u32, level);
        }
        for (n, &volts) in self.analog.iter().enumerate() {
            sim.board.set_analog(n, volts);
        }
    }

    fn load(&mut self, path: PathBuf) {
        let result = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|data| Simulator::new(&data, Some(DEFAULT_FONTS)).map_err(|e| e.to_string()));
        match result {
            Ok(sim) => {
                self.sim = Some(sim);
                self.state = RunState::Running;
                log::info!("loaded {}", path.display());
            }
            Err(e) => {
                self.sim = blank_board();
                self.state = RunState::Error(format!("cannot load {}: {e}", path.display()));
            }
        }
        self.adopt_sim();
        self.core_halted = false;
        self.restore_inputs();
        self.firmware = Some(path);
        self.uploaded = false;
        self.published_generation = u64::MAX;
        self.reanchor();
    }

    /// Runs one slice and keeps virtual time in step with the wall clock.
    fn step(&mut self) {
        let Some(sim) = &mut self.sim else { return };
        let stop = sim.run_for_ns(SLICE.as_nanos() as u64);
        if self.execution == Execution::Running {
            // The debugger decides what happens after a stop.
            if let Some(gdb) = &mut self.gdb {
                gdb.report_stop(&mut sim.machine, &stop);
            }
            self.pace();
            return;
        }
        match stop {
            StopReason::Budget
            | StopReason::DebugBreakpoint { .. }
            | StopReason::Step
            | StopReason::Watchpoint { .. }
            | StopReason::Halted { .. } => {}
            StopReason::Breakpoint { pc } => {
                self.state = RunState::Stopped(format!("BKPT at {}", sim.describe_pc(pc)));
            }
            StopReason::Lockup { pc, reason } => {
                self.state = RunState::Stopped(format!("core locked up at {}: {reason}", sim.describe_pc(pc)));
            }
        }
        self.pace();
    }

    /// Keeps virtual time in step with the wall clock.
    fn pace(&mut self) {
        if let Speed::Factor(f) = self.speed {
            let (wall0, virt0) = self.anchor;
            let virtual_elapsed = Duration::from_nanos(self.virtual_ns() - virt0).as_secs_f64();
            let target = wall0 + Duration::from_secs_f64(virtual_elapsed / f);
            let now = Instant::now();
            if target > now {
                std::thread::sleep(target - now);
            } else if now - target > Duration::from_millis(250) {
                // Too slow to keep up: do not try to catch up later.
                self.reanchor();
            }
        }
    }

    fn publish(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now - self.last_publish < PUBLISH_INTERVAL {
            return;
        }
        self.last_publish = now;
        let virt = self.virtual_ns();
        let dt = (now - self.rate_sample.0).as_secs_f64();
        if dt >= 0.5 {
            self.measured_speed = (virt.saturating_sub(self.rate_sample.1)) as f64 / 1e9 / dt;
            self.rate_sample = (now, virt);
        }

        // Render outside the lock: the UI thread waits for it.
        let new_frame = self.sim.as_ref().and_then(|sim| {
            let b = &sim.board;
            let generation = b.display_generation() ^ (b.backlight().to_bits() as u64);
            if generation == self.published_generation {
                return None;
            }
            if self.ideal_colors {
                b.render_display(&mut self.display);
            } else {
                b.render_display_as_seen(&mut self.display);
            }
            Some(generation)
        });

        let mut snap = self.snapshot.lock().unwrap();
        snap.firmware = if self.uploaded {
            Some("uploaded by debugger".to_owned())
        } else {
            self.firmware.as_ref().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
        };
        snap.state = self.state.clone();
        snap.virtual_ns = virt;
        snap.speed = if self.state == RunState::Running { self.measured_speed } else { 0.0 };
        snap.gdb = self.gdb_status.clone();
        snap.gdb_attached = self.gdb.as_ref().is_some_and(GdbServer::has_controller);
        snap.usb = self.usb_status();
        if let Some(sim) = &self.sim {
            let b = &sim.board;
            if let Some(generation) = new_frame {
                std::mem::swap(&mut snap.display, &mut self.display);
                snap.display_generation = snap.display_generation.wrapping_add(1);
                self.published_generation = generation;
            }
            snap.leds = b.leds();
            snap.dac1 = sim.board.dac1();
            let duty = sim.take_led_duty();
            snap.led_duty = duty.out;
            snap.nucleo_duty = duty.nucleo;
            snap.inputs = b.inputs();
            snap.nucleo_leds = b.nucleo_leds();
            snap.backlight = b.backlight();
            snap.core_mhz = b.mcu().clocks.get().hclk / 1_000_000;
            let out = b.mcu().usart3.borrow_mut().take_output();
            snap.serial.extend(out);
        }
        drop(snap);
        (self.repaint)();
    }
}

fn blank_board() -> Option<Simulator> {
    Simulator::blank(Some(DEFAULT_FONTS)).map_err(|e| log::warn!("cannot create the board: {e}")).ok()
}
