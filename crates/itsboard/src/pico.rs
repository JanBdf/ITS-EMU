//! A Raspberry Pi Pico running a UF2 firmware, as a [`plugin`](crate::plugin)
//! on a thread of its own. Its USB serial port is a pseudo terminal that
//! programs on the host open like the real Pico's port.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use rp2040::Pico;

use crate::plugin::Link;

/// Virtual time the Pico runs between looks at its serial port (ps).
const CHUNK_PS: u64 = 1_000_000_000;
/// Serial output kept for [`PicoPlugin::take_output`] (the oldest is dropped).
const MAX_OUTPUT: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PicoPluginError {
    #[error("Pico: {0}")]
    Pico(#[from] rp2040::PicoError),
    #[error("Pico serial port: {0}")]
    Io(#[from] std::io::Error),
    #[error("reading {0}: {1}")]
    Firmware(PathBuf, std::io::Error),
    #[error("Pico boot ROM: {0}")]
    Bootrom(#[from] rp2040::bootrom::BootromError),
}

/// Wiring of the square wave generator of the Drehgeber exercise: its
/// outputs A0, A5 (GP0, GP5) to PF0, PF1.
pub const DEFAULT_WIRING: [(u8, (char, u32)); 2] = [(0, ('F', 0)), (5, ('F', 1))];

/// A Pico as given on the command line: `FIRMWARE.uf2[@GP0=PF0,GP5=PF1,...]`.
#[derive(Debug, Clone, PartialEq)]
pub struct PicoSpec {
    pub firmware: PathBuf,
    /// Pico GPIO and the board pin it is wired to.
    pub wiring: Vec<(u8, (char, u32))>,
}

impl PicoSpec {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (file, wiring) = match s.rsplit_once('@') {
            Some((file, wires)) => (file, wires.split(',').map(parse_wire).collect::<Result<Vec<_>, _>>()?),
            None => (s, DEFAULT_WIRING.to_vec()),
        };
        if file.is_empty() {
            return Err("expected FIRMWARE.uf2[@GP0=PF0,...]".to_owned());
        }
        Ok(Self { firmware: PathBuf::from(file), wiring })
    }

    /// Starts the Pico (boot ROM from `bootrom`, else see [`rp2040::bootrom::locate`]).
    pub fn start(&self, bootrom: Option<&Path>) -> Result<PicoPlugin, PicoPluginError> {
        let firmware =
            std::fs::read(&self.firmware).map_err(|e| PicoPluginError::Firmware(self.firmware.clone(), e))?;
        let rom = rp2040::bootrom::locate(bootrom)?;
        PicoPlugin::start(rom, firmware, self.wiring.iter().map(|&(gp, _)| gp).collect())
    }

    /// The board pin of each output of the started Pico's link.
    pub fn board_pins(&self) -> Vec<Option<(char, u32)>> {
        self.wiring.iter().map(|&(_, pin)| Some(pin)).collect()
    }
}

impl std::fmt::Display for PicoSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let wires: Vec<String> =
            self.wiring.iter().map(|&(gp, pin)| format!("GP{gp}={}", crate::spec::pin_name(pin))).collect();
        write!(f, "{}@{}", self.firmware.display(), wires.join(","))
    }
}

/// `GP5=PF1` -> (5, ('F', 1))
fn parse_wire(s: &str) -> Result<(u8, (char, u32)), String> {
    let (gp, pin) = s.split_once('=').ok_or_else(|| format!("expected GPn=PIN, got {s:?}"))?;
    let gp = gp.trim().to_ascii_uppercase();
    let gpio = gp
        .strip_prefix("GP")
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|&n| usize::from(n) < rp2040::pins::NUM_GPIO)
        .ok_or_else(|| format!("expected a Pico GPIO such as GP5, got {gp:?}"))?;
    let pin = crate::spec::parse_pin(pin).map_err(|_| format!("expected a board pin such as PF0, got {pin:?}"))?;
    Ok((gpio, pin))
}

pub struct PicoPlugin {
    link: Arc<Link>,
    /// GPIOs whose changes the link carries, in output order.
    gpios: Vec<u8>,
    port: Option<PathBuf>,
    input: mpsc::Sender<Vec<u8>>,
    /// What the Pico said and how it ended, shared with its thread.
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PicoPlugin {
    /// Starts a Pico with `bootrom` (16 KiB) and `uf2` in its flash. The
    /// link reports the changes of `gpios` (output `n` is `gpios[n]`).
    pub fn start(bootrom: Vec<u8>, uf2: Vec<u8>, gpios: Vec<u8>) -> Result<Self, PicoPluginError> {
        // fail here, not in the thread, for a bad file
        Pico::new(&bootrom, &uf2)?;
        let tty = Pty::open()?;
        let port = tty.as_ref().map(|t| t.path.clone());
        let link = Link::new(gpios.len());
        let stop = Arc::new(AtomicBool::new(false));
        let (input, input_rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let thread = {
            let (link, stop, gpios, status) = (link.clone(), stop.clone(), gpios.clone(), status.clone());
            std::thread::Builder::new().name("pico".into()).spawn(move || {
                if let Err(e) = run(&bootrom, &uf2, &gpios, &link, &stop, tty, &input_rx, &status) {
                    log::error!("{e}");
                    status.lock().unwrap().failure = Some(e);
                }
                link.end();
            })?
        };
        Ok(Self { link, gpios, port, input, status, stop, thread: Some(thread) })
    }

    pub fn link(&self) -> Arc<Link> {
        self.link.clone()
    }

    /// The GPIO of each output of the link.
    pub fn gpios(&self) -> &[u8] {
        &self.gpios
    }

    /// The serial port (pseudo terminal) host programs open, if there is one
    /// (not on Windows).
    pub fn serial_port(&self) -> Option<&std::path::Path> {
        self.port.as_deref()
    }

    /// Sends bytes to the Pico's serial port as a host program would.
    pub fn send(&self, data: &[u8]) {
        let _ = self.input.send(data.to_vec());
    }

    /// The Pico's serial output since the last call (also when a program
    /// on the serial port reads it).
    pub fn take_output(&self) -> Vec<u8> {
        std::mem::take(&mut self.status.lock().unwrap().output)
    }

    /// Why the Pico stopped, if it did.
    pub fn failure(&self) -> Option<String> {
        self.status.lock().unwrap().failure.clone()
    }

    /// What the firmware has used that the emulator does not do (DMA, PWM,
    /// programming its flash, ...): it may not work as on a real Pico.
    pub fn notices(&self) -> Vec<String> {
        self.status.lock().unwrap().notices.clone()
    }
}

impl Drop for PicoPlugin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.link.end();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
struct Status {
    output: Vec<u8>,
    failure: Option<String>,
    /// What the firmware used that is not emulated.
    notices: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn run(
    bootrom: &[u8],
    uf2: &[u8],
    gpios: &[u8],
    link: &Link,
    stop: &AtomicBool,
    mut tty: Option<Pty>,
    input: &mpsc::Receiver<Vec<u8>>,
    status: &Mutex<Status>,
) -> Result<(), String> {
    let mut pico = Pico::new(bootrom, uf2).map_err(|e| format!("Pico: {e}"))?;
    let mut line = Vec::new();
    let mut report = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if report.elapsed() >= Duration::from_secs(1) {
            report = std::time::Instant::now();
            log::debug!("Pico at {:.3} s, {}", pico.time_ps() as f64 / 1e12, link.describe());
        }
        // the serial port
        {
            let mut serial = pico.serial().borrow_mut();
            while let Ok(data) = input.try_recv() {
                serial.to_device.extend(data);
            }
            if let Some(t) = &mut tty {
                t.read_into(&mut serial.to_device);
            }
            let out = std::mem::take(&mut serial.from_device);
            drop(serial);
            for &b in &out {
                match b {
                    b'\n' => {
                        log::info!("Pico serial: {}", String::from_utf8_lossy(&line).trim_end());
                        line.clear();
                    }
                    _ => line.push(b),
                }
            }
            if let Some(t) = &mut tty {
                t.write(&out);
            }
            if !out.is_empty() {
                let kept = &mut status.lock().unwrap().output;
                kept.extend_from_slice(&out);
                kept.drain(..kept.len().saturating_sub(MAX_OUTPUT));
            }
        }

        let now = pico.time_ps();
        let target = link.plugin_target(now, Duration::from_millis(5)).min(now + CHUNK_PS);
        if target <= now {
            continue;
        }
        if let Err((core, reason)) = pico.run_until_ps(target) {
            return Err(format!("Pico core {core} stopped at {:#010x}: {reason:?}", pico.pc(core)));
        }
        let notices = pico.take_notices();
        if !notices.is_empty() {
            status.lock().unwrap().notices.extend(notices);
        }
        let edges = pico.take_edges().into_iter().filter_map(|e| {
            let output = gpios.iter().position(|&g| g == e.pin)?;
            Some((e.ps, output, e.level))
        });
        link.publish(pico.time_ps(), edges);
    }
    Ok(())
}

#[cfg(unix)]
struct Pty {
    master: std::fs::File,
    /// Kept open so the master does not see a hang-up while no program has
    /// the port open.
    _slave: std::fs::File,
    path: PathBuf,
    /// Output the port could not take yet.
    pending: Vec<u8>,
}

#[cfg(unix)]
impl Pty {
    fn open() -> std::io::Result<Option<Self>> {
        use std::os::fd::FromRawFd;
        let (mut master, mut slave) = (0, 0);
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: valid out pointers; `name` is large enough (macOS: 128)
        let r = unsafe {
            libc::openpty(&mut master, &mut slave, name.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just opened and are owned from here on
        let (master, slave) = unsafe { (std::fs::File::from_raw_fd(master), std::fs::File::from_raw_fd(slave)) };
        // SAFETY: `name` is NUL-terminated by openpty
        let path = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned());
        set_raw(&slave)?;
        set_nonblocking(&master)?;
        Ok(Some(Self { master, _slave: slave, path, pending: Vec::new() }))
    }

    fn read_into(&mut self, to: &mut std::collections::VecDeque<u8>) {
        use std::io::Read;
        let mut buf = [0u8; 256];
        while let Ok(n @ 1..) = self.master.read(&mut buf) {
            to.extend(&buf[..n]);
        }
    }

    fn write(&mut self, data: &[u8]) {
        use std::io::Write;
        self.pending.extend_from_slice(data);
        while !self.pending.is_empty() {
            match self.master.write(&self.pending) {
                Ok(n @ 1..) => {
                    self.pending.drain(..n);
                }
                _ => break,
            }
        }
        // nobody reads: keep only the latest output (a program opening the
        // port discards what is waiting in the terminal)
        const KEEP: usize = 4096;
        if self.pending.len() > KEEP {
            self.pending.drain(..self.pending.len() - KEEP);
        }
    }
}

#[cfg(unix)]
fn set_raw(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: plain termios calls on an open descriptor
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(f.as_raw_fd(), &mut t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut t);
        if libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_nonblocking(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl on an open descriptor
    unsafe {
        let flags = libc::fcntl(f.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(f.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// No pseudo terminals: the serial port is only reachable through
/// [`PicoPlugin::send`] and the log.
#[cfg(not(unix))]
struct Pty {
    path: PathBuf,
}

#[cfg(not(unix))]
impl Pty {
    fn open() -> std::io::Result<Option<Self>> {
        Ok(None)
    }
    fn read_into(&mut self, _to: &mut std::collections::VecDeque<u8>) {}
    fn write(&mut self, _data: &[u8]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        let s = PicoSpec::parse("gen.uf2").unwrap();
        assert_eq!((s.firmware, s.wiring), (PathBuf::from("gen.uf2"), DEFAULT_WIRING.to_vec()));
        let s = PicoSpec::parse("a@b.uf2@GP2=pg0, gp3=PG1").unwrap();
        assert_eq!((s.firmware, s.wiring), (PathBuf::from("a@b.uf2"), vec![(2, ('G', 0)), (3, ('G', 1))]));
        assert!(PicoSpec::parse("x.uf2@GP30=PF0").is_err());
        assert!(PicoSpec::parse("x.uf2@GP1=F0").is_err());
    }
}
