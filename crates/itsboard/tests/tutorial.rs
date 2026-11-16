//! The GUI's tutorial program sees every input of the board.

mod common;

use common::Sim;

#[test]
fn tutorial_reports_each_input() {
    let Some(mut sim) = Sim::boot("Tutorial.axf") else { return };
    // GUI_init and TP_Init take about 2.7 s
    sim.run_until_ms(3_500);
    let start = sim.take_serial();
    assert!(start.starts_with("Tutorial:"), "{start}");
    // open pads do not report changes by themselves
    assert_eq!(start.lines().count(), 1, "{start}");
    let mut t = 3_500;
    let mut step = |sim: &mut Sim, set: &dyn Fn(&Sim)| {
        set(sim);
        t += 100;
        sim.run_until_ms(t);
        sim.take_serial()
    };

    assert_eq!(step(&mut sim, &|s| s.board.set_button(3, true)), "S3 pressed\n");
    assert_eq!(sim.board.leds() & 0xFF, 1 << 3);
    assert_eq!(step(&mut sim, &|s| s.board.set_button(3, false)), "S3 released\n");
    assert_eq!(step(&mut sim, &|s| s.board.set_user_button(true)), "USER pressed\n");
    assert_eq!(step(&mut sim, &|s| s.board.set_user_button(false)), "USER released\n");
    assert_eq!(step(&mut sim, &|s| s.board.set_int(2, Some(false))), "AUX2 (PG2): GND\n");
    assert_eq!(step(&mut sim, &|s| s.board.set_int(2, Some(true))), "AUX2 (PG2): 3V3\n");
    assert_eq!(step(&mut sim, &|s| s.board.set_int(2, None)), "AUX2 (PG2): open\n");
    // turning the mouse wheel (a step per 100 ms): the display and the LEDs
    // follow at once, the console gets the voltage once it holds still (300 ms)
    let mut out = String::new();
    for v in [1.7, 1.75, 1.8, 1.85, 2.1] {
        out += &step(&mut sim, &|s| s.board.set_analog(5, Some(v)));
    }
    assert_eq!(out, "");
    // 2.1 V of 3.3 V: 5 of the 8 LEDs OUT8-15
    assert_eq!(sim.board.leds() >> 8, 0x1F);
    for _ in 0..4 {
        out += &step(&mut sim, &|_| ());
    }
    assert_eq!(out, "ADC3_8 (PF10): 2.10 V\n");
    let out = step(&mut sim, &|s| s.board.set_touch(Some((400.0, 150.0))));
    assert_eq!(out, "touch 400 150\n");
    step(&mut sim, &|s| s.board.set_touch(None));
    sim.send_serial("hello\n");
    sim.run_until_ms(t + 100);
    assert_eq!(sim.take_serial(), "you typed: hello\n");
}
