//! The GUI's tutorial program leads through every input on the display:
//! each step moves on once its input was used, and reports it on the serial port.

mod common;

use common::Sim;

/// Where Next / Skip and Back are touched.
const NEXT: (f32, f32) = (424.0, 305.0);
const BACK: (f32, f32) = (50.0, 305.0);

struct Run {
    sim: Sim,
    t: u64,
    serial: String,
}

impl Run {
    /// Runs `ms` more, collecting the serial output.
    fn run(&mut self, ms: u64) {
        self.t += ms;
        self.sim.run_until_ms(self.t);
        self.serial += &self.sim.take_serial();
    }

    /// Runs until the program reports `line` (drawing a step takes 0.7 s, a
    /// done step stays 1.8 s); the output up to it.
    fn until(&mut self, line: &str) -> String {
        for _ in 0..40 {
            if let Some(i) = self.serial.find(line) {
                let out = self.serial[..i].to_owned();
                self.serial.drain(..i + line.len());
                return out;
            }
            self.run(100);
        }
        panic!("no {line:?} by {} ms, got {:?}", self.t, self.serial);
    }

    fn touch(&mut self, at: (f32, f32)) {
        self.sim.board.set_touch(Some(at));
        self.run(100);
        self.sim.board.set_touch(None);
        self.run(100);
    }
}

#[test]
fn tutorial_leads_through_each_input() {
    let Some(sim) = Sim::boot("Tutorial.axf") else { return };
    let mut r = Run { sim, t: 0, serial: String::new() };
    // GUI_init and TP_Init take about 2.7 s
    assert_eq!(
        r.until("Step 1/9: Welcome\n"),
        "Tutorial: follow the steps on the display; every input is reported here.\n"
    );
    r.run(800);
    // open pads do not report changes by themselves
    assert_eq!(r.serial, "");
    r.touch(NEXT);
    assert_eq!(r.until("Step 2/9: Buttons S0-S7\n"), "touch 424 305\n");
    r.run(800);

    // a touch is not a button press
    r.touch((150.0, 200.0));
    r.run(2_000);
    assert_eq!(r.serial, "touch 150 200\n");
    r.serial.clear();
    r.sim.board.set_button(3, true);
    r.run(100);
    // pressed lights OUT3
    assert_eq!(r.sim.board.leds() & 0xFF, 1 << 3);
    r.sim.board.set_button(3, false);
    assert_eq!(r.until("Step 3/9: Two buttons at once\n"), "S3 pressed\nS3 released\n");
    r.run(800);
    r.sim.board.set_button(1, true);
    r.run(100);
    r.sim.board.set_button(2, true);
    r.run(100);
    r.sim.board.set_button(1, false);
    r.sim.board.set_button(2, false);
    assert_eq!(r.until("Step 4/9: USER button\n"), "S1 pressed\nS2 pressed\nS1 released\nS2 released\n");

    // Back and Next again
    r.run(800);
    r.touch(BACK);
    assert_eq!(r.until("Step 3/9: Two buttons at once\n"), "touch 50 305\n");
    r.run(800);
    r.touch(NEXT);
    assert_eq!(r.until("Step 4/9: USER button\n"), "touch 424 305\n");
    r.run(800);

    r.sim.board.set_user_button(true);
    r.run(100);
    r.sim.board.set_user_button(false);
    assert_eq!(r.until("Step 5/9: Digital inputs AUX0-5\n"), "USER pressed\nUSER released\n");
    r.run(800);
    // GND is not the goal, 3V3 is
    r.sim.board.set_int(2, Some(false));
    r.run(2_000);
    r.sim.board.set_int(2, Some(true));
    r.run(100);
    r.sim.board.set_int(2, None);
    assert_eq!(r.until("Step 6/9: Analog inputs\n"), "AUX2 (PG2): GND\nAUX2 (PG2): 3V3\nAUX2 (PG2): open\n");
    r.run(800);

    // turning the mouse wheel (a step per 100 ms): the LEDs follow at once,
    // the console gets the voltage once it holds still (300 ms)
    for v in [1.7, 1.75, 1.8, 1.85, 2.1] {
        r.sim.board.set_analog(5, Some(v));
        r.run(100);
    }
    // 2.1 V of 3.3 V: 5 of the 8 LEDs OUT8-15
    assert_eq!(r.sim.board.leds() >> 8, 0x1F);
    assert_eq!(r.until("Step 7/9: Touch screen\n"), "ADC3_8 (PF10): 2.10 V\n");
    r.run(800);

    // Skip and the Back button are not in the touch box
    r.touch(BACK);
    assert_eq!(r.until("Step 6/9: Analog inputs\n"), "touch 50 305\n");
    r.run(800);
    r.touch(NEXT);
    assert_eq!(r.until("Step 7/9: Touch screen\n"), "touch 424 305\n");
    r.run(800);
    r.touch((120.0, 200.0));
    assert_eq!(r.until("Step 8/9: Serial console\n"), "touch 120 200\n");
    r.run(800);

    r.sim.send_serial("hello\n");
    assert_eq!(r.until("Step 9/9: Your own program\n"), "you typed: hello\n");
    r.run(800);
    r.touch(NEXT);
    assert_eq!(r.until("Tutorial done: every input on one screen.\n"), "touch 424 305\n");
}
