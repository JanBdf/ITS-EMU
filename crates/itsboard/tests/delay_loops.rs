//! Counting delay loops (AC6, -O0) give the same cycles whether the
//! emulator skips them or runs every iteration.

mod common;
use common::Sim;

fn run(skipping: bool) -> Option<(String, u64)> {
    let mut sim = Sim::boot("DelayLoops.axf")?;
    sim.machine.system_mut().idle_skipping = skipping;
    sim.run_until_ms(3_200);
    let out = sim.take_serial();
    assert!(out.contains("DelayLoops done"), "{out}");
    Some((out, sim.machine.system().idle_skipped_cycles()))
}

#[test]
fn delay_loops_take_the_same_cycles_when_skipped() {
    let Some((skipped, skipped_cycles)) = run(true) else { return };
    let (plain, _) = run(false).unwrap();
    assert_eq!(skipped, plain);
    assert_eq!(skipped.lines().filter(|l| l.starts_with("D ")).count(), 30);
    // the loops take about 2.4 s of the 3.2 s
    assert!(skipped_cycles > 180_000_000 * 2, "skipped {skipped_cycles} cycles");
}
