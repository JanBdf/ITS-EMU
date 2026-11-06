//! A Pico plugin wired to the board: the square wave generator of the
//! Drehgeber exercise (its UF2 file is not part of the repository: set
//! `ITS_EMU_PICO_UF2`; the boot ROM must be in the cache, see `rp2040::bootrom`).

mod common;

use common::Sim;
use itsboard::pico::PicoPlugin;

/// Boots `program` with a generator on PF0 (GP0) and PF1 (GP5), or `None`
/// without the UF2 file or the boot ROM.
fn with_generator(program: &str) -> Option<(Sim, PicoPlugin)> {
    let Ok(uf2) = std::env::var("ITS_EMU_PICO_UF2").map(std::fs::read) else {
        eprintln!("skipping: set ITS_EMU_PICO_UF2 to pio_2_squarewaves.uf2");
        return None;
    };
    let Ok(rom) = rp2040::bootrom::load(&rp2040::bootrom::cache_path()) else {
        eprintln!("skipping: no RP2040 boot ROM in the cache");
        return None;
    };
    let sim = Sim::boot(program)?;
    let pico = PicoPlugin::start(rom, uf2.unwrap(), vec![0, 5]).unwrap();
    sim.board.add_plugin(pico.link(), &[Some(('F', 0)), Some(('F', 1))]);
    Some((sim, pico))
}

/// The whole path without third-party files (runs everywhere): a Pico on
/// its thread with the stand-in boot ROM and the blink firmware, GP0 wired
/// to PF0, seen by the board's program at the Pico's pace.
#[test]
fn a_pico_on_its_thread_drives_a_board_pin() {
    use rp2040::testfw::{blink_uf2, stub_bootrom};
    let mut sim = Sim::boot("HwCheck.axf").expect("HwCheck.axf is published");
    let pico = PicoPlugin::start(stub_bootrom(), blink_uf2(), vec![0]).unwrap();
    sim.board.add_plugin(pico.link(), &[Some(('F', 0))]);

    // GP0 toggles every 462 µs (3003 cycles at the ROSC's 6.5 MHz)
    sim.run_until_ms(5);
    let level = |sim: &Sim| sim.board.inputs() & 1;
    let mut last = level(&sim);
    let mut changes = 0;
    for _ in 0..(20_000 / 50) {
        sim.machine.run(sim.machine.clock().ns_to_cycles(50_000));
        let now = level(&sim);
        changes += u32::from(now != last);
        last = now;
    }
    assert!((42..=45).contains(&changes), "20 ms / 462 µs: {changes} changes");
    assert_eq!(pico.failure(), None);
}

#[test]
fn square_wave_generator_drives_the_buttons_pins() {
    let Some((mut sim, pico)) = with_generator("HwCheck.axf") else { return };

    // the generator's USB serial port: 360° at 3600 °/s = 1200 phase changes in 0.1 s
    sim.run_until_ms(600);
    pico.send(b"h");
    sim.run_until_ms(1_000);
    pico.send(b"s3600\r\n");
    sim.run_until_ms(1_400);
    pico.send(b"r");

    // PF0/PF1 sampled every 20 µs (a phase lasts 83 µs)
    let level = |sim: &Sim| sim.board.inputs() & 3;
    let mut last = level(&sim);
    let (mut changes, mut both, mut steps) = (0, 0, std::collections::HashMap::new());
    for _ in 0..(400_000 / 20) {
        sim.machine.run(sim.machine.clock().ns_to_cycles(20_000));
        let now = level(&sim);
        if now != last {
            changes += 1;
            if now ^ last == 3 {
                both += 1;
            }
            *steps.entry((last, now)).or_insert(0) += 1;
            last = now;
        }
    }
    assert_eq!(changes, 1200, "one per phase change: {steps:?}");
    assert_eq!(both, 0, "one signal changes at a time (quadrature)");
    // (PF1 PF0) 00 -> 10 -> 11 -> 01: phases a, b, c, d of the exercise
    assert_eq!(steps.len(), 4, "one direction: four distinct steps {steps:?}");
    assert!(steps.contains_key(&(0b00, 0b10)), "a positive angle turns a -> b: {steps:?}");
}

/// A 32-bit variable of the program.
fn variable(sim: &Sim, name: &str) -> i32 {
    let addr = sim.image.symbol(name).unwrap_or_else(|| panic!("no symbol {name}")).addr;
    sim.machine.read_u32(addr).unwrap() as i32
}

#[test]
fn quadrature_decoder_counts_the_generators_phase_changes() {
    let Some((mut sim, pico)) = with_generator("PicoQuadrature.axf") else { return };

    // the board library's initialisation takes about 2 s
    let mut ms = 0;
    let mut serial = String::new();
    while !serial.contains("PicoQuadrature ready") {
        ms += 100;
        assert!(ms < 5_000, "no ready message: {serial:?}");
        sim.run_until_ms(ms);
        serial += &sim.take_serial();
    }
    // the generator answers each command after about 300 ms
    let mut command = |sim: &mut Sim, text: &[u8], wait: u64| {
        pico.send(text);
        ms += wait;
        sim.run_until_ms(ms);
    };
    // connecting drives the outputs low one after the other (the board pulls
    // them up before): 11 -> 10 -> 00, two steps backwards
    command(&mut sim, b"h", 400);
    assert_eq!((variable(&sim, "count"), variable(&sim, "errors")), (-2, 0));
    // a full turn forwards at 3600 °/s: 1200 phase changes in 0.1 s
    command(&mut sim, b"s3600\r\n", 400);
    command(&mut sim, b"r", 600);
    assert_eq!((variable(&sim, "count"), variable(&sim, "errors")), (1198, 0));
    assert_eq!(variable(&sim, "dir"), 1);
    // 90° backwards at 720 °/s
    command(&mut sim, b"a-90\r\n", 400);
    command(&mut sim, b"s720\r\n", 400);
    command(&mut sim, b"r", 800);
    assert_eq!((variable(&sim, "count"), variable(&sim, "errors")), (898, 0));
    assert_eq!(variable(&sim, "dir"), -1);
    // and its report on the serial port
    command(&mut sim, b"", 600);
    let serial = sim.take_serial();
    assert!(serial.contains("count 898 angle 269.4"), "{serial:?}");
}
