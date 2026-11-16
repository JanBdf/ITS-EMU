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
fn register_reset_values_match_the_board() {
    // all peripheral clocks on, then the registers of timers, SPIs, U(S)ARTs,
    // I2Cs, ADC, DAC, SYSCFG and EXTI (first line of the file: the commands)
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-regs.txt")).unwrap();
    let (cmds, expected) = board.split_once('\n').unwrap();
    let Some(mut sim) = Sim::boot("HwCheck.axf") else { return };
    sim.run_until_ms(2_500);
    assert!(sim.take_serial().ends_with("ready\n"));
    let mut out = String::new();
    for (i, cmd) in cmds.trim_start_matches("# ").split(" | ").enumerate() {
        sim.send_serial(&format!("{cmd}\n"));
        sim.run_until_ms(2_600 + 100 * i as u64);
        out += &sim.take_serial();
    }
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("R ")).collect();
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(emu.len(), expected.len(), "{out}");
    let differ: Vec<String> =
        emu.iter().zip(&expected).filter(|(e, b)| e != b).map(|(e, b)| format!("{e} (board {b})")).collect();
    assert!(differ.is_empty(), "{differ:#?}");
}

#[test]
fn dma_matches_the_board() {
    // memory-to-memory copy, ADC1 scan into memory (single, after the end
    // of the transfer, continuous + circular): exact except the samples
    let Some(out) = run("dma", 2_000) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-dma.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("D ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        if b.contains("_buf ") {
            let values = |l: &str| -> Vec<i64> { l.split_whitespace().skip(2).map(|v| v.parse().unwrap()).collect() };
            for (x, y) in values(e).iter().zip(values(b)) {
                assert!((x - y).abs() <= 8, "{e} / board {b}");
            }
        } else {
            assert_eq!(e, b);
        }
    }
}

#[test]
fn usart_transmission_by_dma_matches_the_board() {
    let Some(out) = run("dmatx", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-dmatx.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| !l.starts_with("ok ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        if b.starts_with("X tc_us") {
            // durations in µs: until the last byte was handed over, until it was sent
            let us = |l: &str| -> Vec<f64> { [2, 4].map(|i| l.split(' ').nth(i).unwrap().parse().unwrap()).to_vec() };
            for (x, y) in us(e).iter().zip(us(b)) {
                assert!((x - y).abs() <= 5.0, "{e} / board {b}");
            }
        } else {
            assert_eq!(e, b);
        }
    }
}

#[test]
fn spi_transfer_by_dma_matches_the_board() {
    // SPI3 with nothing on the header: the open MISO reads 0
    let Some(out) = run("dmaspi", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-dmaspi.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("S ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu[..2], board[..2]);
    let cycles = |l: &str| -> f64 { l.rsplit(' ').next().unwrap().parse().unwrap() };
    assert!((cycles(emu[2]) - cycles(board[2])).abs() <= 40.0, "{} / board {}", emu[2], board[2]);
}

#[test]
fn adc_timer_trigger_matches_the_board() {
    // ADC1 started by TIM3 TRGO at 1 kHz (and by the UG at the start)
    let Some(out) = run("adctrig", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-adctrig.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("G ")).collect();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn dac_waves_and_trigger_match_the_board() {
    // noise/triangle generation by software triggers, TIM6 TRGO with DMA, underrun
    let Some(out) = run("dacwave", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-dacwave.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("W ")).collect();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn timer_outputs_match_the_board() {
    // PC6 driven by TIM3/TIM8 CH1: PWM levels against CNT, forced levels,
    // stale OCxREF after a mode change, capture of the own toggling output
    let Some(out) = run("timout", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timout.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("O ")).collect();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn slave_modes_match_the_board() {
    // TIM3 external clock 1, reset, gated and trigger mode with TI1/TI2 on
    // PC6/PC7 (edges from switching the pulls). The open pin needs about 1 µs
    // to rise on the board: counts after an edge may be 1 lower there.
    let Some(out) = run("timslave", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timslave.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("S ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        let (et, bt): (Vec<&str>, Vec<&str>) = (e.split(' ').collect(), b.split(' ').collect());
        assert_eq!(et.len(), bt.len(), "{e} / {b}");
        for k in 0..et.len() {
            let after_edge = k > 0 && (bt[k - 1] == "after" || (bt[1] == "trigger" && bt[k - 1] == "cnt"));
            if after_edge {
                let (x, y): (i64, i64) = (et[k].parse().unwrap(), bt[k].parse().unwrap());
                assert!((x - y).abs() <= 1, "{e} / {b}");
            } else {
                assert_eq!(et[k], bt[k], "{e} / {b}");
            }
        }
    }
}

#[test]
fn counting_directions_match_the_board() {
    // TIM3 up, down and center-aligned 1/2/3: PWM levels per direction, flags
    let Some(out) = run("timdir", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timdir.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("O ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        let (ev, bv): (Vec<&str>, Vec<&str>) = (e.split_whitespace().collect(), b.split_whitespace().collect());
        if bv[2] == "uif" && (bv[1] == "up" || bv[1] == "down") {
            // The 10th update comes within a few cycles of the end of the
            // 10 ms window: 9 or 10 on the board, by code layout.
            let n = |v: &[&str]| -> i64 { v[3].parse().unwrap() };
            assert!((n(&ev) - n(&bv)).abs() <= 1, "{e}\n{b}");
            assert_eq!(ev[4..], bv[4..], "{e}\n{b}");
        } else {
            assert_eq!(e, b);
        }
    }
}

#[test]
fn ccr_preload_matches_the_board() {
    // TIM3_CH1 PWM on PC6: CCR written mid-period with and without OC1PE
    let Some(out) = run("timpre", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timpre.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("O ")).collect();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn timer_chaining_matches_the_board() {
    // TIM3 slave modes with TIM2's TRGO (ITR1)
    let Some(out) = run("timchain", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timchain.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("C ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        if b.starts_with("C trigger") {
            // The lag includes the time between two CNT reads, in which APB
            // reads separated by other instructions are a few cycles short.
            let lag = |l: &str| -> i64 { l.split_whitespace().nth(7).unwrap().parse().unwrap() };
            assert!((lag(e) - lag(b)).abs() <= 2, "{e}\n{b}");
        } else {
            assert_eq!(e, b);
        }
    }
}

#[test]
fn timer_chaining_delays_match_the_board() {
    // Gate length, start offsets and when UG or a reset by TRGO reaches CNT
    let Some(out) = run("timsync", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timsync.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("Y ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    let num = |l: &str, i: usize| -> i64 { l.split_whitespace().nth(i).unwrap().parse().unwrap() };
    for (e, b) in emu.iter().zip(&board) {
        let name = b.split_whitespace().nth(1).unwrap();
        if name == "trigger" || name == "software" {
            // CNT of two timers read one after the other: the slave's offset
            // ((a + b) / 2) matches, the reads' distance is short (APB timing)
            let offset = |l: &str| num(l, 3) + num(l, 5);
            assert!((offset(e) - offset(b)).abs() <= 2, "{e}\n{b}");
        } else if b.contains(" t ") {
            // CNT samples after a write: the first still old (or the written
            // value), the second at the reset's count; later ones and the
            // cycle stamps depend on the APB timing.
            let (s0e, s0b) = (num(e, 2), num(b, 2));
            assert_eq!(s0e >= 100, s0b >= 100, "{e}\n{b}");
            assert!((num(e, 3) - num(b, 3)).abs() <= 1, "{e}\n{b}");
        } else {
            assert_eq!(e, b);
        }
    }
}

#[test]
fn encoder_mode_matches_the_board() {
    // TIM8 encoder modes on PC6/PC7, quadrature made by switching the pulls
    let Some(out) = run("enc", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-enc.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("Q ")).collect();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn interrupt_latency_matches_the_board() {
    // Cycles from the pending write to the handler's first instruction. Only
    // with interrupts masked until CPSIE: the emulator takes interrupts at the
    // end of a translated block, so the unmasked case depends on the layout.
    let Some(out) = run("irqlat", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-irqlat.txt")).unwrap();
    let lines = |text: &str| -> Vec<(String, f64)> {
        text.lines()
            .filter(|l| l.starts_with("L ") && l.contains("_masked"))
            .map(|l| {
                let v: Vec<&str> = l.split_whitespace().collect();
                (v[1].to_owned(), v[2].parse().unwrap())
            })
            .collect()
    };
    let (emu, board) = (lines(&out), lines(&board));
    assert_eq!(emu.len(), 6, "{out}");
    for ((name, x), (b_name, y)) in emu.iter().zip(&board) {
        assert_eq!(name, b_name);
        assert!((x - y).abs() <= 2.0, "{name}: emulator {x}, board {y}");
    }
}

#[test]
fn watchdog_resets_like_the_board() {
    // HwCheck `iwdg 2 fff`: refreshed once, then the board resets after
    // 1.92 s (last line "W t 1920000"); the reset flags read IWDG + PIN.
    let Some(mut sim) = Sim::boot("HwCheck.axf") else { return };
    sim.run_until_ms(2_500);
    assert!(sim.take_serial().ends_with("ready\n"));
    sim.send_serial("iwdg 2 fff\n");
    sim.run_until_ms(7_000);
    let out = sim.take_serial();
    assert!(out.contains("W t 1920000\n") && !out.contains("W t 1930000"), "{out}");
    assert!(out.ends_with("ready\n"), "{out}");
    sim.send_serial("iwdg 2 fff\n");
    sim.run_until_ms(7_100);
    assert!(sim.take_serial().starts_with("W csr 24000000\n"));
}

#[test]
fn i2c_matches_the_board() {
    // I2C1 with nothing on the header: flags exact, durations within 2 %
    // (the shortest ones are one iteration of the polling loop)
    let Some(out) = run("i2c", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-i2c.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("I ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    for (e, b) in emu.iter().zip(&board) {
        if b.contains("_cycles") {
            let value = |l: &str| -> f64 { l.rsplit(' ').next().unwrap().parse().unwrap() };
            let (x, y) = (value(e), value(b));
            assert!((x - y).abs() <= (y * 0.02).max(40.0), "{e} / board {b}");
        } else {
            assert_eq!(e, b);
        }
    }
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
        } else if name.starts_with("cont") || name.starts_with("vrefint") || name == "temp" || name == "vbat" {
            3.0
        } else {
            0.0
        };
        assert!((x - y).abs() <= tol, "{name}: emulator {e:?}, board {b:?}");
        if b.len() == 4 && (name.starts_with("dac") || ["vrefint", "temp", "vbat"].contains(&name)) {
            // the noise: min to max of 16 conversions
            let spread = |v: &[String]| -> f64 { v[3].parse::<f64>().unwrap() - v[1].parse::<f64>().unwrap() };
            let (x, y) = (spread(e), spread(b));
            assert!(x >= y / 3.0 && x <= y * 2.0 + 2.0, "{name} noise: emulator {e:?}, board {b:?}");
        }
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

#[test]
fn timer_dma_requests_match_the_board() {
    // TIM2 update/CC1 requests on DMA1, DMAR bursts, a request while the stream is off
    let Some(out) = run("timdma", 500) else { return };
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("T ")).collect();
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-timdma.txt")).unwrap();
    assert_eq!(emu, board.lines().collect::<Vec<_>>());
}

#[test]
fn debug_ids_match_the_board() {
    // ROM table, CoreSight component IDs, DBGMCU_IDCODE, CPUID and the
    // processor feature registers, TPIU and ETM registers (HwCheck `rd`)
    let Some(mut sim) = Sim::boot("HwCheck.axf") else { return };
    sim.run_until_ms(2_500);
    assert!(sim.take_serial().ends_with("ready\n"));
    let board = std::fs::read_to_string(format!("{REFERENCE}/probe-ids.txt")).unwrap();
    let mut ms = 2_500;
    for line in board.lines() {
        let addr = line.split_whitespace().nth(1).unwrap();
        sim.send_serial(&format!("rd {addr}\n"));
        ms += 5;
        sim.run_until_ms(ms);
        let out = sim.take_serial();
        assert!(out.lines().any(|l| l == line), "board: {line}, emulator: {out}");
    }
}

#[test]
fn flash_erase_and_programming_times_match_the_board() {
    // Sector 23 erased at x32 and x8, 64 accesses of each size programmed
    let Some(out) = run("flash", 3_500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-flash.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("F ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    let num = |l: &str, i: usize| -> f64 { l.split_whitespace().nth(i).unwrap().parse().unwrap() };
    for (e, b) in emu.iter().zip(&board) {
        if b.contains("erase") {
            // the board's erase times vary by about 10 % from run to run
            assert!((num(e, 2) / num(b, 2) - 1.0).abs() < 0.12, "{e}\n{b}");
            assert_eq!(
                e.split_whitespace().skip(3).collect::<Vec<_>>(),
                b.split_whitespace().skip(3).collect::<Vec<_>>()
            );
        } else if b.contains("prog") {
            assert!((num(e, 3) / num(b, 3) - 1.0).abs() < 0.02, "{e}\n{b}");
        } else {
            assert_eq!(e, b);
        }
    }
}

#[test]
fn clock_start_up_times_match_the_board() {
    // HSI, HSE in bypass and main PLL switched off and on again
    let Some(out) = run("rcc", 500) else { return };
    let board = std::fs::read_to_string(format!("{REFERENCE}/HwCheck-rcc.txt")).unwrap();
    let emu: Vec<&str> = out.lines().filter(|l| l.starts_with("K ")).collect();
    let board: Vec<&str> = board.lines().collect();
    assert_eq!(emu.len(), board.len(), "{out}");
    assert_eq!(emu[0], board[0], "CR, CFGR, PLLCFGR");
    let ns = |l: &str, name: &str| -> f64 {
        let v: Vec<&str> = l.split_whitespace().collect();
        let i = v.iter().position(|&w| w == name).unwrap();
        v[i + 1].parse().unwrap()
    };
    // switching off is the polling loop's time (not modelled); on is the start-up
    // (HSI: 0.97-1.07 us on the board, polled in steps of about 0.1 us)
    for (i, name, tol) in [(1, "hsi_on", 0.15), (2, "pll_on", 0.03), (2, "again", 0.03), (3, "hse_on", 0.03)] {
        let (x, y) = (ns(emu[i], name), ns(board[i], name));
        assert!((x / y - 1.0).abs() < tol, "{name}: emulator {x} ns, board {y} ns");
    }
}
