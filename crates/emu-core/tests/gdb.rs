//! GDB server tests against the clang-built self-test firmware.
#![cfg(feature = "gdb")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use emu_core::gdb::{Execution, GdbServer};
use emu_core::{Bus, ElfImage, Machine, MachineConfig, MemoryMap};

fn fixture_path(name: &str) -> String {
    format!("{}/../../tests/fixtures/micro/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn config() -> MachineConfig {
    MachineConfig {
        memory: MemoryMap {
            flash_base: 0x0800_0000,
            flash_size: 2 * 1024 * 1024,
            flash_alias_at_zero: true,
            ram: vec![(0x2000_0000, 192 * 1024), (0x1000_0000, 64 * 1024)],
            rom_zero: vec![(0x1FFF_0000, 0x8000), (0x1FFF_C000, 0x1000)],
            sram_bitband: Some((0x2000_0000, 0x2200_0000)),
            periph_base: 0x4000_0000,
            periph_size: 0x2000_0000,
        },
        reset_cpu_hz: 16_000_000,
        quantum: 100_000,
        masked_quantum: 1_000,
    }
}

/// Emulator thread: like the GUI, it polls the server between slices.
struct Emulator {
    port: u16,
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Emulator {
    fn start() -> Self {
        Self::with_firmware(Some("exceptions.elf"))
    }

    /// Without firmware the flash is empty, as on a new board.
    fn with_firmware(firmware: Option<&'static str>) -> Self {
        let _ = env_logger::builder().is_test(true).try_init();
        let (tx, rx) = channel();
        let quit = Arc::new(AtomicBool::new(false));
        let q = quit.clone();
        let thread = std::thread::spawn(move || {
            let mut m = Machine::new(config(), Bus::new()).unwrap();
            if let Some(name) = firmware {
                m.load_elf(&ElfImage::parse(&std::fs::read(fixture_path(name)).unwrap()).unwrap()).unwrap();
            }
            m.reset();
            let mut server = GdbServer::bind("127.0.0.1:0").unwrap();
            tx.send(server.local_addr().unwrap().port()).unwrap();
            while !q.load(Ordering::Relaxed) {
                match server.poll(&mut m) {
                    Execution::Running => {
                        let stop = m.run(20_000);
                        server.report_stop(&mut m, &stop);
                    }
                    // Wait for the debugger (like `its-emu --gdb`).
                    Execution::Free | Execution::Halted => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        });
        Self { port: rx.recv().unwrap(), quit, thread: Some(thread) }
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Values of `print` commands (`$1 = ...`) in GDB's output, in order.
fn printed_values(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with('$'))
        .filter_map(|l| l.split_once(" = ").map(|(_, v)| v.trim().to_string()))
        .collect()
}

/// A GDB that can debug Arm targets: `$ITS_EMU_GDB`, `arm-none-eabi-gdb`,
/// `gdb-multiarch` or a `gdb` built with all targets.
fn arm_gdb() -> Option<String> {
    let candidates =
        std::env::var("ITS_EMU_GDB").into_iter().chain(["arm-none-eabi-gdb", "gdb-multiarch", "gdb"].map(String::from));
    candidates.into_iter().find(|gdb| {
        Command::new(gdb)
            .args(["-nx", "-batch", "-ex", "set architecture armv7e-m"])
            .output()
            .is_ok_and(|o| o.status.success() && o.stderr.is_empty())
    })
}

#[test]
fn gdb_session_with_breakpoints_steps_and_reset() {
    let Some(gdb) = arm_gdb() else {
        eprintln!("no GDB with Arm support found (set ITS_EMU_GDB); skipping");
        return;
    };
    let emu = Emulator::start();
    let out = Command::new(gdb)
        .args(["-nx", "-batch", "-q"])
        .args(["-ex", &format!("target remote 127.0.0.1:{}", emu.port)])
        .args(["-ex", "print/x $pc"])
        .args(["-ex", "print $pc == &Reset_Handler"])
        .args(["-ex", "break SysTick_Handler"])
        .args(["-ex", "continue"])
        .args(["-ex", "print results.ticks"])
        .args(["-ex", "continue"])
        .args(["-ex", "print results.ticks"])
        .args(["-ex", "delete"])
        .args(["-ex", "print/x $pc"])
        .args(["-ex", "stepi"])
        .args(["-ex", "print $pc != &SysTick_Handler"])
        .args(["-ex", "print $sp == $msp"])
        .args(["-ex", "print $s0 == $s0"])
        .args(["-ex", "watch results.ticks"])
        .args(["-ex", "continue"])
        .args(["-ex", "delete"])
        .args(["-ex", "set var results.magic = 0x55"])
        .args(["-ex", "print/x results.magic"])
        .args(["-ex", "monitor reset halt"])
        .args(["-ex", "maintenance flush register-cache"])
        .args(["-ex", "print $pc == &Reset_Handler"])
        .args(["-ex", "detach"])
        .arg(fixture_path("exceptions.elf"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v = printed_values(&stdout);
    let msg = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(v.len(), 10, "{msg}");
    assert_eq!(v[1], "1", "halted at reset on connect; {msg}");
    assert!(stdout.contains("Breakpoint 1, SysTick_Handler"), "{msg}");
    let ticks: u32 = v[2].parse().unwrap();
    assert_eq!(v[3].parse::<u32>().unwrap(), ticks + 1, "{msg}");
    assert_eq!(v[5], "1", "stepi moved the pc; {msg}");
    assert_eq!(v[6], "1", "sp is msp in handler mode; {msg}");
    assert_eq!(v[7], "1", "FP registers readable; {msg}");
    assert_eq!(v[8], "0x55", "memory write; {msg}");
    assert!(stdout.contains("atchpoint 2: results.ticks"), "watchpoint set; {msg}");
    assert!(stdout.contains(&format!("Old value = {}\nNew value = {}", ticks + 1, ticks + 2)), "watchpoint hit; {msg}");
    assert!(msg.contains("Resetting target"), "monitor output; {msg}");
    assert_eq!(v[9], "1", "monitor reset halt; {msg}");
}

// --- raw protocol ---------------------------------------------------------

struct Client(TcpStream);

impl Client {
    fn connect(port: u16) -> Self {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Client(s)
    }

    fn send(&mut self, data: &str) {
        let sum = data.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        write!(self.0, "${data}#{sum:02x}").unwrap();
    }

    /// Next packet payload (acks are skipped and every packet is acked).
    fn recv(&mut self) -> String {
        let mut payload = Vec::new();
        let mut byte = [0u8];
        loop {
            self.0.read_exact(&mut byte).unwrap();
            if byte[0] == b'$' {
                break;
            }
        }
        loop {
            self.0.read_exact(&mut byte).unwrap();
            if byte[0] == b'#' {
                break;
            }
            payload.push(byte[0]);
        }
        let mut sum = [0u8; 2];
        self.0.read_exact(&mut sum).unwrap();
        self.0.write_all(b"+").unwrap();
        // Run-length encoding: "X*n" repeats X (n - 29) more times.
        let mut out = Vec::new();
        let mut it = payload.into_iter();
        while let Some(b) = it.next() {
            if b == b'*' {
                let last = *out.last().unwrap();
                let n = it.next().unwrap() - 29;
                out.extend(std::iter::repeat_n(last, n as usize));
            } else {
                out.push(b);
            }
        }
        String::from_utf8(out).unwrap()
    }

    fn request(&mut self, data: &str) -> String {
        self.send(data);
        self.recv()
    }
}

/// Little-endian u32 from a hex reply.
fn le_u32(hex: &str) -> u32 {
    let bytes: Vec<u8> = (0..4)
        .map(|i| {
            u8::from_str_radix(hex.get(2 * i..2 * i + 2).unwrap_or("??"), 16)
                .unwrap_or_else(|_| panic!("reply {hex:?}"))
        })
        .collect();
    u32::from_le_bytes(bytes.try_into().unwrap())
}

#[test]
fn observer_reads_while_controller_runs() {
    let emu = Emulator::start();
    let mut ctl = Client::connect(emu.port);
    let reply = ctl.request("?");
    assert!(reply.starts_with('T') || reply.starts_with('S'), "{reply}");
    // An endless loop in RAM: `adds r0, #1; b .-2`, started with r0 = 0.
    assert_eq!(ctl.request("M2000f000,4:0130fde7"), "OK");
    assert_eq!(ctl.request("Pf=00f00020"), "OK");
    assert_eq!(ctl.request("P0=00000000"), "OK");
    assert_eq!(le_u32(&ctl.request("pf")), 0x2000_F000);
    ctl.send("c");

    // A second client (live watch) reads registers while the target runs.
    let mut obs = Client::connect(emu.port);
    assert!(!obs.request("?").is_empty());
    let first = le_u32(&obs.request("p0"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut later = first;
    while later == first && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        later = le_u32(&obs.request("p0"));
    }
    assert!(later > first, "target does not run: r0 {first} -> {later}");
    // Its resume requests do not move the target.
    assert!(!obs.request("s").is_empty());

    // Ctrl-C from the controller stops the target.
    ctl.0.write_all(&[0x03]).unwrap();
    let stop = ctl.recv();
    assert!(stop.starts_with("T02") || stop.starts_with("S02"), "stop reply {stop}");
    let halted = le_u32(&obs.request("p0"));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(le_u32(&obs.request("p0")), halted, "target halted");
    let pc = le_u32(&ctl.request("pf"));
    assert!((0x2000_F000..0x2000_F004).contains(&pc), "pc {pc:#x} inside the loop");
}

#[test]
fn monitor_load_flashes_a_blank_board() {
    let Some(gdb) = arm_gdb() else {
        eprintln!("no GDB with Arm support found (set ITS_EMU_GDB); skipping");
        return;
    };
    let emu = Emulator::with_firmware(None);
    let out = Command::new(gdb)
        .args(["-nx", "-batch", "-q"])
        .args(["-ex", &format!("target remote 127.0.0.1:{}", emu.port)])
        .args(["-ex", "monitor load /nonexistent.elf"])
        .args(["-ex", &format!("monitor load {}", fixture_path("exceptions.elf"))])
        .args(["-ex", "maintenance flush register-cache"])
        .args(["-ex", "print $pc == &Reset_Handler"])
        .args(["-ex", "break SysTick_Handler"])
        .args(["-ex", "continue"])
        .args(["-ex", "detach"])
        .arg(fixture_path("exceptions.elf"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let msg = format!("stdout:\n{stdout}\nstderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(msg.contains("cannot load '/nonexistent.elf'"), "{msg}");
    assert!(msg.contains("target reset and halted"), "{msg}");
    assert_eq!(printed_values(&stdout), ["1"], "pc at the reset vector of the loaded file; {msg}");
    assert!(stdout.contains("Breakpoint 1, SysTick_Handler"), "loaded program runs; {msg}");
}
