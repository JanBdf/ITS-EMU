//! The emulator runs on its own thread: `Simulator` is not `Send` (Unicorn,
//! shared peripheral state), so it is created and used only there. The UI
//! sends [`Command`]s and reads the latest [`Snapshot`].

use std::path::PathBuf;
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
    Quit,
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
/// changed. With `gdb_addr`, a GDB server listens there.
pub fn spawn(repaint: impl Fn() + Send + 'static, gdb_addr: Option<String>) -> EmuHandle {
    let (tx, rx) = channel();
    let snapshot = SharedSnapshot::default();
    let shared = snapshot.clone();
    let thread = std::thread::Builder::new()
        .name("emulator".into())
        .spawn(move || Worker::new(rx, shared, Box::new(repaint), gdb_addr).run())
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
    /// How the debugger wants the core to run (`Free` without debugger).
    execution: Execution,
}

impl Worker {
    fn new(
        commands: Receiver<Command>,
        snapshot: SharedSnapshot,
        repaint: Box<dyn Fn() + Send>,
        gdb_addr: Option<String>,
    ) -> Self {
        let now = Instant::now();
        let (gdb, gdb_status) = match gdb_addr {
            None => (None, Err("off".to_owned())),
            Some(addr) => match GdbServer::bind(addr.as_str()) {
                Ok(server) => {
                    let addr = server.local_addr().map_or(addr, |a| a.to_string());
                    log::info!("GDB server on {addr}");
                    (Some(server), Ok(addr))
                }
                Err(e) => {
                    log::warn!("cannot start GDB server on {addr}: {e}");
                    (None, Err(format!("{addr}: {e}")))
                }
            },
        };
        Self {
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
            gdb,
            gdb_status,
            execution: Execution::Free,
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
            if self.state == RunState::Running {
                self.step();
            }
            self.publish(false);
        }
    }

    fn handle(&mut self, cmd: Command) {
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
            Command::Quit => {}
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
        if loaded.is_some() || sim.machine.flash_writes() != flash_writes {
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
