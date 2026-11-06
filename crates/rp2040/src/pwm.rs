//! PWM: eight slices of a 16-bit counter with two outputs each (A on GPIO
//! 2n, B on GPIO 2n + 1 of slice n mod 8, function 4).
//!
//! The counter counts clk_sys (the CPU clock) through the 8.4 fractional
//! divider, free-running (0..=TOP) or phase-correct (0..=TOP..=0); an output
//! is high while the counter is below its compare value (inverted by
//! A_INV/B_INV). TOP and CC are double-buffered: taken at the wrap, or at
//! once while the slice is off. Wraps set INTR (PWM_IRQ_WRAP). Output
//! changes are put on the pins at their time when the PWM is brought up to
//! date (on an access, and when the Pico's peripherals are updated). The
//! DIVMODE inputs on the B pins, PH_ADV/PH_RET and the DMA requests are not
//! modelled (noted when used).

use std::rc::Rc;

use emu_core::{Clock, DeviceCtx};

use crate::pins::SharedPins;
use crate::regs::{Block, Notices, Op};

pub const PWM_IRQ_WRAP: u32 = 4;
const CSR_EN: u32 = 1 << 0;
const CSR_PH_CORRECT: u32 = 1 << 1;

#[derive(Clone, Copy)]
struct Slice {
    csr: u32,
    div: u32,
    top: u32,
    cc: u32,
    /// TOP and CC as compared (latched at the wrap).
    top_active: u32,
    cc_active: u32,
    /// Position in the period (0..period) and the time of that tick, in
    /// sixteenths of a cycle (the divider's resolution).
    phase: u32,
    base16: u64,
}

impl Default for Slice {
    fn default() -> Self {
        Self { csr: 0, div: 0x10, top: 0xFFFF, cc: 0, top_active: 0xFFFF, cc_active: 0, phase: 0, base16: 0 }
    }
}

impl Slice {
    fn running(&self) -> bool {
        self.csr & CSR_EN != 0
    }

    fn phase_correct(&self) -> bool {
        self.csr & CSR_PH_CORRECT != 0
    }

    /// Clock divider in sixteenths (DIV_INT = 0 divides by 256).
    fn div16(&self) -> u64 {
        let int = u64::from((self.div >> 4) & 0xFF);
        (if int == 0 { 256 } else { int }) * 16 + u64::from(self.div & 0xF)
    }

    fn period(&self) -> u32 {
        (self.top_active + 1) * if self.phase_correct() { 2 } else { 1 }
    }

    fn counter(&self, phase: u32) -> u32 {
        let top = self.top_active;
        if self.phase_correct() && phase > top { 2 * top + 1 - phase } else { phase }
    }

    /// Levels of outputs A and B (bits 0, 1) at `phase`.
    fn outputs(&self, phase: u32) -> u32 {
        let ctr = self.counter(phase);
        (0..2).fold(0, |acc, ch| {
            let high = ctr < (self.cc_active >> (16 * ch)) & 0xFFFF;
            let inv = self.csr & (4 << ch) != 0;
            acc | (u32::from(high != inv) << ch)
        })
    }

    /// Next phase after the current one at which an output may change or
    /// the counter wraps (`period()`).
    fn next_point(&self) -> u32 {
        let period = self.period();
        let mut next = period;
        for ch in 0..2 {
            let cc = (self.cc_active >> (16 * ch)) & 0xFFFF;
            let points = if self.phase_correct() { [cc, period.saturating_sub(cc)] } else { [cc, period] };
            for p in points {
                if p > self.phase && p < next {
                    next = p;
                }
            }
        }
        next
    }

    /// Cycle at which the counter is `ticks` ticks after the base.
    fn cycle_after(&self, ticks: u64) -> u64 {
        (self.base16 + ticks * self.div16()).div_ceil(16)
    }

    /// Ticks after the base up to `now`.
    fn ticks_to(&self, now: u64) -> u64 {
        (now * 16).saturating_sub(self.base16) / self.div16()
    }

    /// The counter restarts its tick at `now` (after a change of its settings).
    fn rebase(&mut self, now: u64) {
        self.base16 = now * 16;
    }

    /// Brings the counter to its position at `now`. Each output change and
    /// wrap is passed to `on` (cycle, outputs, wrapped).
    fn advance(&mut self, now: u64, mut on: impl FnMut(u64, u32, bool)) {
        if !self.running() {
            self.rebase(now);
            return;
        }
        loop {
            let point = self.next_point();
            let ticks = u64::from(point - self.phase);
            let at = self.cycle_after(ticks);
            if at > now {
                break;
            }
            let wrap = point == self.period();
            let before = self.outputs(self.phase);
            self.base16 += ticks * self.div16();
            self.phase = if wrap { 0 } else { point };
            if wrap {
                (self.top_active, self.cc_active) = (self.top, self.cc);
            }
            let after = self.outputs(self.phase);
            if wrap || after != before {
                on(at, after, wrap);
            }
            // whole periods without output changes at once
            if wrap && after == self.outputs(self.period() - 1) && self.next_point() == self.period() {
                let periods = self.ticks_to(now) / u64::from(self.period());
                if periods > 0 {
                    self.base16 += periods * u64::from(self.period()) * self.div16();
                    on(self.cycle_after(0), after, true);
                }
            }
        }
        // the ticks since the last point (whole ticks: the base moves with them)
        let ticks = self.ticks_to(now);
        self.base16 += ticks * self.div16();
        self.phase += ticks as u32;
    }

    /// Cycle of the next wrap (if the slice runs).
    fn next_wrap(&self) -> Option<u64> {
        self.running().then(|| self.cycle_after(u64::from(self.period() - self.phase)))
    }
}

pub struct Pwm {
    slices: [Slice; 8],
    intr: u8,
    inte: u8,
    intf: u8,
    pins: SharedPins,
    notices: Notices,
    noted: bool,
}

impl Pwm {
    pub fn new(pins: SharedPins, notices: Notices) -> Self {
        Self { slices: [Slice::default(); 8], intr: 0, inte: 0, intf: 0, pins, notices, noted: false }
    }

    fn note(&mut self, what: &str) {
        if !self.noted {
            self.noted = true;
            self.notices.note(format!("The firmware uses PWM {what}, which is not emulated"));
        }
    }

    /// Brings every slice up to now: output changes go to the pins, wraps to INTR.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        let pins = Rc::clone(&self.pins);
        let mut wrapped = 0u8;
        for (n, s) in self.slices.iter_mut().enumerate() {
            s.advance(now, |at, out, wrap| {
                if wrap {
                    wrapped |= 1 << n;
                }
                let mut p = pins.borrow_mut();
                let old = p.pwm_out;
                p.pwm_out = (old & !(3 << (2 * n))) | (out << (2 * n));
                if p.pwm_out != old {
                    p.refresh(ctx.clock.ps_at_cycle(at));
                }
            });
        }
        if wrapped != 0 {
            self.intr |= wrapped;
            self.sync_irq(ctx);
        }
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        ctx.set_irq(PWM_IRQ_WRAP, (self.intr | self.intf) & self.inte != 0);
    }

    /// Sets the outputs of slice `n` to its levels now (after a change of
    /// its configuration).
    fn put_outputs(&self, ctx: &DeviceCtx, n: usize) {
        let s = &self.slices[n];
        let out = s.outputs(s.phase);
        let mut p = self.pins.borrow_mut();
        let new = (p.pwm_out & !(3 << (2 * n))) | (out << (2 * n));
        if new != p.pwm_out {
            p.pwm_out = new;
            p.refresh(ctx.clock.time_ps());
        }
    }
}

impl Block for Pwm {
    fn name(&self) -> &str {
        "PWM"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32 {
        self.advance(ctx);
        match off {
            0x00..=0x9C => {
                let s = &self.slices[(off / 0x14) as usize];
                match off % 0x14 {
                    0x0 => s.csr,
                    0x4 => s.div,
                    0x8 => s.counter(s.phase),
                    0xC => s.cc,
                    _ => s.top,
                }
            }
            0xA0 => self.slices.iter().enumerate().fold(0, |acc, (n, s)| acc | (s.csr & CSR_EN) << n),
            0xA4 => u32::from(self.intr),
            0xA8 => u32::from(self.inte),
            0xAC => u32::from(self.intf),
            0xB0 => u32::from((self.intr | self.intf) & self.inte),
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        self.advance(ctx);
        let now = ctx.now();
        match off {
            0x00..=0x9C => {
                let n = (off / 0x14) as usize;
                let s = &mut self.slices[n];
                match off % 0x14 {
                    0x0 => {
                        let v = op.apply(s.csr, value);
                        let (divmode, strobes) = ((v >> 4) & 3, v & 0xC0);
                        s.csr = v & 0x3F;
                        if s.phase_correct() && s.phase >= s.period() {
                            s.phase = s.counter(s.phase);
                        }
                        s.rebase(now);
                        if divmode != 0 {
                            self.note("with a DIVMODE input on a B pin");
                        } else if strobes != 0 {
                            self.note("phase advance/retard (PH_ADV, PH_RET)");
                        }
                    }
                    0x4 => {
                        s.div = op.apply(s.div, value) & 0xFFF;
                        s.rebase(now);
                    }
                    0x8 => {
                        s.phase = op.apply(s.counter(s.phase), value) & 0xFFFF;
                        s.rebase(now);
                    }
                    0xC => s.cc = op.apply(s.cc, value),
                    _ => s.top = op.apply(s.top, value) & 0xFFFF,
                }
                let s = &mut self.slices[n];
                if !s.running() {
                    (s.top_active, s.cc_active) = (s.top, s.cc);
                }
                s.phase = s.phase.min(s.period() - 1);
                self.put_outputs(ctx, n);
            }
            0xA0 => {
                let en = op.apply(self.read(ctx, 0xA0), value);
                for n in 0..8 {
                    let s = &mut self.slices[n];
                    let on = en & (1 << n) != 0;
                    if on != s.running() {
                        s.csr ^= CSR_EN;
                        s.rebase(now);
                        if !on {
                            (s.top_active, s.cc_active) = (s.top, s.cc);
                        }
                    }
                }
            }
            0xA4 => self.intr &= !(op.clears(value) as u8),
            0xA8 => self.inte = op.apply(u32::from(self.inte), value) as u8,
            0xAC => self.intf = op.apply(u32::from(self.intf), value) as u8,
            _ => {}
        }
        self.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.advance(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        // only a wrap that raises the interrupt needs the PWM on time
        let waiting = self.inte & !self.intr;
        (0..8).filter(|&n| waiting & (1 << n) != 0).filter_map(|n| self.slices[n].next_wrap()).min()
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        match off {
            0x00..=0x9C if off % 0x14 == 0x8 => clock.cycles(),
            0xA4 | 0xB0 => {
                (0..8).filter_map(|n| self.slices[n].next_wrap()).min().unwrap_or(u64::MAX).max(clock.cycles())
            }
            _ => u64::MAX,
        }
    }

    fn reset(&mut self) {
        let (pins, notices) = (self.pins.clone(), self.notices.clone());
        *self = Self::new(pins, notices);
        self.pins.borrow_mut().pwm_out = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pins::PinEdge;
    use emu_core::nvic::Nvic;

    struct Rig {
        pwm: Pwm,
        pins: SharedPins,
        clock: Clock,
        nvic: Nvic,
    }

    impl Rig {
        fn new() -> Self {
            let pins = SharedPins::default();
            {
                let mut p = pins.borrow_mut();
                p.ctrl[0] = crate::pins::FUNC_PWM; // slice 0 A
                p.ctrl[1] = crate::pins::FUNC_PWM; // slice 0 B
            }
            Self {
                pwm: Pwm::new(pins.clone(), Notices::default()),
                pins,
                clock: Clock::new(125_000_000),
                nvic: Nvic::new(),
            }
        }
        fn write(&mut self, off: u32, value: u32) {
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.pwm.write(&mut ctx, off, Op::Write, value);
        }
        fn read(&mut self, off: u32) -> u32 {
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.pwm.read(&mut ctx, off)
        }
        fn run_to(&mut self, cycle: u64) {
            self.clock.advance(cycle - self.clock.cycles());
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.pwm.update(&mut ctx);
        }
        /// (cycle, pin, level) of the pin changes so far.
        fn edges(&mut self) -> Vec<(u64, u8, bool)> {
            let edges = self.pins.borrow_mut().take_edges();
            edges.iter().map(|&PinEdge { ps, pin, level }| (self.clock.cycle_at_ps(ps), pin, level.unwrap())).collect()
        }
    }

    #[test]
    fn free_running_duty_cycle_on_both_outputs() {
        let mut r = Rig::new();
        r.write(0x10, 9); // TOP: period 10
        r.write(0x0C, 3 | 7 << 16); // A: 3, B: 7
        r.write(0x00, CSR_EN);
        assert_eq!(r.edges(), [(0, 0, true), (0, 1, true)]);
        r.run_to(25);
        assert_eq!(
            r.edges(),
            [
                (3, 0, false),
                (7, 1, false),
                (10, 0, true),
                (10, 1, true),
                (13, 0, false),
                (17, 1, false),
                (20, 0, true),
                (20, 1, true),
                (23, 0, false)
            ]
        );
        assert_eq!(r.read(0x08), 5, "CTR");
        // a new compare value takes effect at the wrap
        r.write(0x0C, 5 | 7 << 16);
        r.run_to(36);
        assert_eq!(r.edges(), [(27, 1, false), (30, 0, true), (30, 1, true), (35, 0, false)]);
    }

    #[test]
    fn divider_phase_correct_and_inversion() {
        let mut r = Rig::new();
        r.write(0x04, 2 << 4 | 8); // 2.5
        r.write(0x10, 3);
        r.write(0x0C, 1);
        r.write(0x00, CSR_EN | CSR_PH_CORRECT | 8); // B inverted: always high (CC_B = 0)
        // counter 0 1 2 3 3 2 1 0 at 2.5 cycles a tick: A high while below 1
        r.run_to(40);
        let edges =
            [(0, 0, true), (0, 1, false), (0, 1, true), (3, 0, false), (18, 0, true), (23, 0, false), (38, 0, true)];
        assert_eq!(r.edges(), edges);
    }

    #[test]
    fn wrap_interrupt() {
        let mut r = Rig::new();
        r.write(0x14 + 0x10, 99); // slice 1, TOP
        r.write(0xA8, 2); // INTE slice 1
        r.write(0xA0, 2); // EN of slice 1
        assert_eq!(r.pwm.next_event(), Some(100));
        r.run_to(100);
        assert_eq!(r.read(0xB0), 2);
        assert!(r.nvic.is_pending(16 + PWM_IRQ_WRAP));
        r.write(0xA4, 2);
        assert_eq!(r.read(0xB0), 0);
        assert_eq!(r.pwm.next_event(), Some(200));
        // long runs without outputs: whole periods at once
        r.run_to(1_000_050);
        assert_eq!(r.read(0x14 + 0x08), 50);
    }
}
