//! External devices on the header pins, driven against HwCheck register probes.

mod common;

use common::Sim;

/// HwCheck `rd`/`wr` over the virtual COM port.
struct Probe {
    sim: Sim,
    ms: u64,
}

impl Probe {
    fn boot() -> Option<Self> {
        let mut sim = Sim::boot("HwCheck.axf")?;
        sim.run_until_ms(2_500);
        assert!(sim.take_serial().ends_with("ready\n"));
        Some(Self { sim, ms: 2_500 })
    }

    fn wait_ms(&mut self, ms: u64) {
        self.ms += ms;
        self.sim.run_until_ms(self.ms);
    }

    fn cmd(&mut self, line: &str) -> String {
        self.sim.send_serial(&format!("{line}\n"));
        self.wait_ms(20);
        let out = self.sim.take_serial();
        assert!(out.ends_with(&format!("ok {}\n", line.split(' ').next().unwrap())), "{line}: {out}");
        out
    }

    fn rd(&mut self, addr: u32) -> u32 {
        let out = self.cmd(&format!("rd {addr:x}"));
        let word = out.split_whitespace().nth(2).unwrap();
        u32::from_str_radix(word, 16).unwrap()
    }

    fn wr(&mut self, addr: u32, value: u32) {
        self.cmd(&format!("wr {addr:x} {value:x}"));
    }

    fn set_bits(&mut self, addr: u32, clear: u32, set: u32) {
        let v = self.rd(addr);
        self.wr(addr, (v & !clear) | set);
    }
}

const RCC_AHB1ENR: u32 = 0x4002_3830;
const RCC_APB2ENR: u32 = 0x4002_3844;
const GPIOC: u32 = 0x4002_0800;
const TIM8: u32 = 0x4001_0400;

#[test]
fn rotary_encoder_counts_in_timer_encoder_mode() {
    let Some(mut p) = Probe::boot() else { return };
    let enc = p.sim.board.add_encoder(('C', 6), ('C', 7));
    p.set_bits(RCC_AHB1ENR, 0, 1 << 2);
    p.set_bits(RCC_APB2ENR, 0, 1 << 1);
    p.set_bits(GPIOC + 0x20, 0xFF << 24, 0x33 << 24); // AF3 = TIM8
    p.set_bits(GPIOC, 0xF << 12, 0xA << 12); // PC6/PC7 alternate function
    p.wr(TIM8 + 0x18, 0x0101); // CC1S = CC2S = 01
    p.wr(TIM8 + 0x2C, 0xFFFF);
    p.wr(TIM8 + 0x08, 3); // encoder mode 3: both edges
    p.wr(TIM8 + 0x24, 1000);
    p.wr(TIM8, 1); // CR1: CEN

    p.sim.board.rotate_encoder(enc, 2).unwrap();
    p.wait_ms(20);
    assert_eq!(p.rd(TIM8 + 0x24), 1008, "two detents clockwise");
    assert_eq!(p.rd(TIM8) & 0x10, 0, "DIR: up");
    p.sim.board.rotate_encoder(enc, -3).unwrap();
    p.wait_ms(20);
    assert_eq!(p.rd(TIM8 + 0x24), 996, "three detents counter-clockwise");
    assert_eq!(p.rd(TIM8) & 0x10, 0x10, "DIR: down");
    // at rest both contacts are open: the pins read high
    assert_eq!(p.rd(GPIOC + 0x10) >> 6 & 3, 3);
}

#[test]
fn rotary_encoder_on_plain_inputs() {
    // polled through IDR: the levels go through the quadrature cycle
    let Some(mut p) = Probe::boot() else { return };
    let enc = p.sim.board.add_encoder(('G', 0), ('G', 1));
    p.set_bits(RCC_AHB1ENR, 0, 1 << 6);
    let gpiog_idr = 0x4002_1810;
    assert_eq!(p.rd(gpiog_idr) & 3, 3);
    p.sim.board.rotate_encoder(enc, 1).unwrap();
    // a transition every millisecond: A falls first (clockwise)
    p.sim.machine.run(90_000); // 0.5 ms
    let a_fell = !p.sim.board.mcu().gpio.borrow().level(6, 0) && p.sim.board.mcu().gpio.borrow().level(6, 1);
    assert!(a_fell, "A low, B still high after the first transition");
    p.wait_ms(10);
    assert_eq!(p.rd(gpiog_idr) & 3, 3, "back at rest after a detent");
}
