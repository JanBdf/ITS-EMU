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
fn input_levels_match_the_board() {
    // IDR of all ports without pull, with pull-down and with pull-up on the
    // input pins: external pull-ups/drivers and floating levels of the board.
    let Some(out) = run("pins", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-pins.txt")).unwrap();
    let lines =
        |text: &str| -> Vec<String> { text.lines().filter(|l| l.starts_with("P ")).map(str::to_owned).collect() };
    let (emu, board) = (lines(&out), lines(&board));
    assert_eq!(emu.len(), board.len());
    for (e, b) in emu.iter().zip(&board) {
        if b.starts_with("P D") {
            // PD8 (USART3 TX, busy printing) and PD15 (backlight PWM) toggle.
            let mask = |l: &str| -> Vec<u32> {
                l.split_whitespace()
                    .skip(2)
                    .collect::<Vec<_>>()
                    .chunks(2)
                    .map(|kv| u32::from_str_radix(kv[1], 16).unwrap())
                    .collect()
            };
            let (me, mb) = (mask(e), mask(b));
            for (i, (x, y)) in me.iter().zip(&mb).enumerate() {
                let ignore = if i >= 4 { 0x8100 } else { 0 };
                assert_eq!(x & !ignore, y & !ignore, "{e}\n{b}");
            }
        } else {
            assert_eq!(e, b);
        }
    }
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

#[test]
fn timers_match_the_board() {
    // compare flags, CC/update interrupt counts, TIM1 repetition counter,
    // basic timers, TIM1/TIM10 on one interrupt line, register masks
    let Some(out) = run("tim", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-tim.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("M ")).collect();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn adc_and_dac_match_the_board() {
    let Some(out) = run("adc", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-adc.txt")).unwrap();
    let lines = |text: &str| -> Vec<Vec<String>> {
        text.lines()
            .filter(|l| l.starts_with("A "))
            .map(|l| l.split_whitespace().skip(1).map(str::to_owned).collect())
            .collect()
    };
    let (emu, board) = (lines(&out), lines(&board));
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        assert_eq!(e[0], b[0]);
        let name = b[0].as_str();
        // the value compared: the mean of min/mean/max statistics
        let value = |v: &[String]| -> f64 { v[if v.len() == 4 { 2 } else { 1 }].parse().unwrap() };
        let (x, y) = (value(e), value(b));
        if name == "pa0_input_pullup" {
            continue; // a digital input with pull-up reads an unstable low value
        }
        let tol = if name.starts_with("conv") || name.starts_with("scan3") {
            12.0 // a polling loop measures the time
        } else if name.starts_with('p') && name.contains("_in") {
            250.0 // open pins pick up noise
        } else if name.starts_with("dac") && !name.contains("dor") && b.len() == 4 || name.starts_with("inj_jdr") {
            15.0 // noise of the converter, VREFINT at short sampling time
        } else if name.starts_with("cont") || name.starts_with("vrefint") {
            3.0
        } else {
            0.0
        };
        assert!((x - y).abs() <= tol, "{name}: emulator {e:?}, board {b:?}");
    }
}

#[test]
fn exti_matches_the_board() {
    let Some(out) = run("exti", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-exti.txt")).unwrap();
    let lines = |text: &str| -> Vec<Vec<String>> {
        text.lines()
            .filter(|l| l.starts_with("E "))
            .map(|l| l.split_whitespace().map(str::to_owned).collect())
            .collect()
    };
    let (emu, board) = (lines(&out), lines(&board));
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        if b[1] == "poll0" {
            // the emulator resolves time per translated block, not per access
            continue;
        }
        if b.contains(&"latency".to_owned()) {
            // interrupt latency in cycles (the board's first one is 13 faster)
            let n = b.len() - 1;
            let (x, y): (f64, f64) = (e[n].parse().unwrap(), b[n].parse().unwrap());
            assert!((x - y).abs() <= 13.0, "{e:?}\n{b:?}");
            assert_eq!(e[..n], b[..n]);
        } else {
            assert_eq!(e, b);
        }
    }
}
