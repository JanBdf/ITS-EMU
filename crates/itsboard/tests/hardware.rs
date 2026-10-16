//! Timing compared with measurements on the real board (tests/hardware).

mod common;

use std::collections::BTreeMap;

use common::Sim;

const REFERENCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/hardware");

/// `<tag> <name> <value>` lines of a HwCheck output.
fn parse(text: &str, tag: &str) -> BTreeMap<String, f64> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            (parts.next()? == tag).then_some(())?;
            Some((parts.next()?.to_owned(), parts.next()?.parse().ok()?))
        })
        .collect()
}

fn reference(file: &str, tag: &str) -> BTreeMap<String, f64> {
    parse(&std::fs::read_to_string(format!("{REFERENCE}/{file}")).unwrap(), tag)
}

/// Runs HwCheck commands; returns the serial output.
fn run(cmd: &str, ms: u64) -> Option<String> {
    let mut sim = Sim::boot("HwCheck.axf")?;
    sim.run_until_ms(2_500);
    assert!(sim.take_serial().ends_with("ready\n"));
    sim.send_serial(&format!("{cmd}\n"));
    sim.run_until_ms(2_500 + ms);
    let out = sim.take_serial();
    assert!(out.ends_with(&format!("ok {cmd}\n")), "{out}");
    Some(out)
}

#[test]
fn library_call_durations_match_the_board() {
    let Some(out) = run("time", 3_000) else {
        return;
    };
    let emu = parse(&out, "T");
    let board = reference("HwCheck-time.txt", "T");
    // Relative tolerance per measurement. GUI_clear is SPI bound; drawing
    // single points still misses a few bus effects; HAL_Delay depends on the
    // SysTick phase (100..101 ms).
    let tolerance = [
        ("clear", 0.001),
        ("str24", 0.02),
        ("circle", 0.10),
        ("delay100", 0.01),
        ("loop100k", 0.001),
        ("asm100k", 0.001),
        ("printf41", 0.001),
    ];
    for (name, tol) in tolerance {
        let (e, b) = (emu[name], board[name]);
        assert!((e - b).abs() <= b * tol, "{name}: emulator {e}, board {b} (tolerance {tol})");
    }
}

#[test]
fn instruction_timing_matches_the_board() {
    let Some(out) = run("cyc", 500) else { return };
    let emu = parse(&out, "C");
    let board = reference("HwCheck-cyc.txt", "C");
    // Known deviations: conditional branches to the next instruction (the
    // taken refill is not visible), neighbour effects of peripheral accesses
    // (AHB loads/stores back to back, isolated APB accesses in one block),
    // one cycle per iteration in store/load mixes, VFP divide/square root
    // stalls (14.7 instead of 15 cycles in dependent chains).
    let known = [
        "bne_taken",
        "bne_not",
        "ldr_ahb1",
        "str_ahb1",
        "ldr_eth",
        "str_eth",
        "ldr_ahb1_nop",
        "ldr_apb1_nop",
        "stio_stsram",
        "stio_alu",
        "ldio_stsram",
        "str_b_ldr",
        "gpio_rmw",
        "gpio_ld_and_st",
        "stio_ldsram",
        "stsram_ldio",
        "stsram_ldsram",
        "str_ldr_same",
        "vdiv",
        "vsqrt",
    ];
    let mut report = String::new();
    let mut failed = Vec::new();
    for (name, &b) in &board {
        let e = emu[name];
        let err = (e - b) / b;
        report += &format!("{name:16} board {b:7} emu {e:7} {:+6.1} %\n", err * 100.0);
        let tol = if known.contains(&name.as_str()) { 1.0 } else { 0.01 };
        // About 40 cycles of call/measurement overhead are not exact.
        if (e - b).abs() > (b * tol).max(60.0) {
            failed.push(name.clone());
        }
    }
    assert!(failed.is_empty(), "{failed:?}\n{report}");
}

#[test]
fn division_timing_matches_the_board() {
    let Some(out) = run("div", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-div.txt")).unwrap();
    let udiv = |text: &str| -> Vec<(String, f64)> {
        text.lines()
            .filter(|l| l.starts_with("D "))
            .map(|l| {
                let p: Vec<&str> = l.split_whitespace().collect();
                (format!("{}/{}", p[1], p[2]), p[3].parse().unwrap())
            })
            .collect()
    };
    let (emu, board) = (udiv(&out), udiv(&board));
    assert_eq!(emu.len(), board.len());
    for ((name, e), (_, b)) in emu.iter().zip(&board) {
        // 100 x 10 divisions; about 40 cycles of call/measurement overhead.
        assert!((e - b).abs() <= 60.0, "UDIV {name}: emulator {e}, board {b}");
    }
}
