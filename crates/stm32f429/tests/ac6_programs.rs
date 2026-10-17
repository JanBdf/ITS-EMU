//! Runs course programs built with Arm Compiler 6 (tests/fixtures/ac6,
//! regenerate with tools/build-fixtures.sh). Lab solutions are not published;
//! tests that need them are skipped if the image is missing.

use emu_core::{ElfImage, Machine, StopReason};
use stm32f429::gpio::port;

fn fixture(name: &str) -> Option<Vec<u8>> {
    let path = format!("{}/../../tests/fixtures/ac6/{name}", env!("CARGO_MANIFEST_DIR"));
    match std::fs::read(&path) {
        Ok(data) => Some(data),
        Err(_) => {
            eprintln!("skipping: {path} not available (run tools/build-fixtures.sh)");
            None
        }
    }
}

/// Boots a fixture, or returns from the test if it is not available.
macro_rules! boot {
    ($name:expr, $idle:expr) => {
        match fixture($name) {
            Some(data) => boot(&data, $idle),
            None => return,
        }
    };
}

fn boot(data: &[u8], idle_skipping: bool) -> (Machine, stm32f429::Stm32f429) {
    let image = ElfImage::parse(data).unwrap();
    let (bus, mcu) = stm32f429::build();
    let mut m = stm32f429::machine(bus, &mcu).unwrap();
    m.load_elf(&image).unwrap();
    m.load_bin(0x0810_0000, &fixture("fonts.bin").expect("fonts.bin")).unwrap();
    m.system_mut().idle_skipping = idle_skipping;
    m.reset();
    (m, mcu)
}

/// Runs until the given virtual time (in ms) is reached.
fn run_until_ms(m: &mut Machine, ms: u64) {
    loop {
        let now = m.clock().time_ns();
        let target = ms * 1_000_000;
        if now >= target {
            return;
        }
        let budget = m.clock().ns_to_cycles(target - now).max(1);
        assert_eq!(m.run(budget), StopReason::Budget, "pc {:#x}", m.pc());
    }
}

/// OUT15..OUT0 of the ITS-Board (PD0–7 = OUT0–7, PE0–7 = OUT8–15).
fn leds(mcu: &stm32f429::Stm32f429) -> u16 {
    let g = mcu.gpio.borrow();
    (0..16)
        .filter(|&i| if i < 8 { g.level(port('D'), i) } else { g.level(port('E'), i - 8) })
        .fold(0, |a, i| a | (1 << i))
}

#[test]
fn init_its_board_reaches_180_mhz() {
    let (mut m, mcu) = boot!("LauflichtASM.axf", true);
    run_until_ms(&mut m, 50);
    assert_eq!(mcu.clocks.get().hclk, 180_000_000);
    assert_eq!(mcu.clocks.get().apb1_timer, 90_000_000);
}

#[test]
fn lauflicht_steps_every_500_ms() {
    // First pattern 0x8000 rotated right once per delay(500).
    let (mut m, mcu) = boot!("LauflichtASM.axf", true);
    for step in 0..8u64 {
        run_until_ms(&mut m, 250 + step * 500);
        assert_eq!(leds(&mcu), 0x8000 >> step, "step {step}");
    }
}

#[test]
fn idle_skipping_does_not_change_behaviour() {
    let (mut a, mcu_a) = boot!("LauflichtASM.axf", true);
    let (mut b, mcu_b) = boot!("LauflichtASM.axf", false);
    for ms in [100, 333, 777, 1500, 2600] {
        run_until_ms(&mut a, ms);
        run_until_ms(&mut b, ms);
        assert_eq!(leds(&mcu_a), leds(&mcu_b), "at {ms} ms");
    }
    assert!(a.system().idle_skipped_cycles() > 0);
}

#[test]
fn idle_skipping_of_final_self_loop_does_not_change_behaviour() {
    // Both programs end in `b .` (a block that branches to itself).
    for name in ["A1ASM.axf", "PrimzahlSiebASM.axf"] {
        let (mut a, mcu_a) = boot!(name, true);
        let (mut b, mcu_b) = boot!(name, false);
        run_until_ms(&mut a, 300);
        run_until_ms(&mut b, 300);
        assert_eq!(leds(&mcu_a), leds(&mcu_b), "{name}");
        assert_eq!(a.pc(), b.pc(), "{name}");
        assert_eq!(a.clock().cycles(), b.clock().cycles(), "{name}");
        assert!(a.system().idle_skipped_cycles() > a.clock().cycles() / 2, "{name}: loop not skipped");
    }
}
