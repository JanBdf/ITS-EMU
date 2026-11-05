//! Runs a UF2 file on the emulated Pico without the board: prints what the
//! firmware sends on its USB serial port and the pin changes.
//!
//! `cargo run --release -p rp2040 --example run_uf2 -- FILE.uf2 SECONDS [INPUT...]`
//! Each INPUT is `TIME:TEXT` (seconds; `\n` is a newline), sent to the serial port.

use rp2040::{Pico, bootrom};

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let uf2 = std::fs::read(&args[1]).expect("UF2 file");
    let seconds: f64 = args.get(2).map_or(1.0, |s| s.parse().unwrap());
    let mut inputs: Vec<(u64, Vec<u8>)> = args[3..]
        .iter()
        .map(|a| {
            let (t, text) = a.split_once(':').unwrap();
            ((t.parse::<f64>().unwrap() * 1e12) as u64, text.replace("\\r", "\r").replace("\\n", "\n").into_bytes())
        })
        .collect();
    inputs.sort_by_key(|i| i.0);
    let rom = bootrom::locate(None).expect("boot ROM");
    let mut pico = Pico::new(&rom, &uf2).expect("Pico");
    let end = (seconds * 1e12) as u64;
    // RUN_UF2_STEP_PS: virtual time between looks at the serial port (default 1 ms)
    let step: u64 = std::env::var("RUN_UF2_STEP_PS").ok().and_then(|s| s.parse().ok()).unwrap_or(1_000_000_000);
    let mut edges = std::collections::BTreeMap::<u8, (usize, u64, u64)>::new();
    let wall = std::time::Instant::now();
    while pico.time_ps() < end {
        let t = pico.time_ps() + step;
        if let Err((core, stop)) = pico.run_until_ps(t) {
            eprintln!("core {core} stopped at {:.6} s: {stop:?}", pico.time_ps() as f64 / 1e12);
            let m = pico.core(core);
            let sp = m.reg(emu_core::CoreReg::Sp);
            let frame: Vec<String> = (0..8).map(|i| format!("{:#010x}", m.read_u32(sp + 4 * i).unwrap_or(0))).collect();
            eprintln!("sp {sp:#010x} frame r0 r1 r2 r3 r12 lr pc psr: {frame:?}");
            break;
        }
        while inputs.first().is_some_and(|i| i.0 <= pico.time_ps()) {
            let (_, text) = inputs.remove(0);
            pico.serial().borrow_mut().to_device.extend(text);
        }
        let out = std::mem::take(&mut pico.serial().borrow_mut().from_device);
        if !out.is_empty() {
            eprintln!("[{:.4}] {:?}", pico.time_ps() as f64 / 1e12, String::from_utf8_lossy(&out));
        }
        for e in pico.take_edges() {
            let s = edges.entry(e.pin).or_insert((0, e.ps, e.ps));
            s.0 += 1;
            s.2 = e.ps;
        }
    }
    eprintln!(
        "\n--- {:.3} s virtual in {:.3} s, pc {:#010x} / {:#010x}",
        pico.time_ps() as f64 / 1e12,
        wall.elapsed().as_secs_f64(),
        pico.pc(0),
        pico.pc(1)
    );
    // RUN_UF2_DUMP=ADDR:WORDS,... dumps memory (hex) at the end
    for spec in std::env::var("RUN_UF2_DUMP").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
        let (addr, n) = spec.split_once(':').unwrap();
        let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16).unwrap();
        let words: Vec<String> = (0..n.parse().unwrap())
            .map(|i| format!("{:08x}", pico.machine().read_u32(addr + 4 * i).unwrap_or(0)))
            .collect();
        eprintln!("{addr:#010x}: {}", words.join(" "));
    }
    for (pin, (n, first, last)) in edges {
        eprintln!("GP{pin}: {n} changes, first {:.6} s, last {:.6} s", first as f64 / 1e12, last as f64 / 1e12);
    }
}
