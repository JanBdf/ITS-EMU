//! A PWM slice drives GPIO 0 while the core idles: every edge reaches the
//! pins at its time.

use rp2040::Pico;
use rp2040::testfw::{pwm_uf2, stub_bootrom};

#[test]
fn pwm_output_has_its_period_and_duty_cycle() {
    let mut pico = Pico::new(&stub_bootrom(), &pwm_uf2()).unwrap();
    pico.run_until_ps(5_000_000_000).expect("runs for 5 ms");
    let edges: Vec<_> = pico.take_edges().into_iter().filter(|e| e.pin == 0).collect();
    let rising: Vec<u64> = edges.iter().filter(|e| e.level == Some(true)).map(|e| e.ps).collect();
    let falling: Vec<u64> = edges.iter().filter(|e| e.level == Some(false)).map(|e| e.ps).collect();
    assert!(rising.len() > 10, "{} periods", rising.len());
    // 1000 cycles a period, high for 250 (to the rounding of cycles to picoseconds)
    let period = rising[2] - rising[1];
    for pair in rising.windows(2).skip(1) {
        assert!((pair[1] - pair[0]).abs_diff(period) <= 1, "{pair:?}");
    }
    let high = falling.iter().find(|&&f| f > rising[1]).unwrap() - rising[1];
    assert!((high * 4).abs_diff(period) <= 4, "high {high} of {period} ps");
    let cycle_ps = period / 1000;
    assert!(cycle_ps > 50, "clk_sys from the ring oscillator: {cycle_ps} ps a cycle");
}
