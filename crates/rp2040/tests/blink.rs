//! A Pico boots a firmware from UF2 and drives a GPIO, with both cores
//! running (core 1 sleeps in the stand-in boot ROM). Needs no third-party
//! files, unlike the tests with the real boot ROM.

use rp2040::Pico;
use rp2040::testfw::{blink_uf2, stub_bootrom};

#[test]
fn blink_toggles_gpio0_at_a_steady_rate() {
    let mut pico = Pico::new(&stub_bootrom(), &blink_uf2()).unwrap();
    pico.run_until_ps(20_000_000_000).expect("both cores run for 20 ms");
    let edges: Vec<_> = pico.take_edges().into_iter().filter(|e| e.pin == 0).collect();
    assert!(edges.len() > 20, "GPIO0 toggles: {} edges", edges.len());
    for pair in edges.windows(2) {
        assert_eq!(pair[1].level, pair[0].level.map(|l| !l), "alternating levels: {pair:?}");
    }
    // each period is the same instructions: the same time (to the rounding
    // of cycles to picoseconds)
    let periods: Vec<_> = edges.windows(2).skip(1).map(|p| p[1].ps - p[0].ps).collect();
    assert!(periods.iter().all(|&p| p.abs_diff(periods[0]) <= 1), "steady: {periods:?}");
    assert_eq!(pico.outputs()[0], edges.last().unwrap().level);
    assert!(pico.pc(1) < 0x10, "core 1 stays in the boot ROM's WFE loop: {:#x}", pico.pc(1));
}
