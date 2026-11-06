//! ADC: 12-bit conversions of GPIO 26..29 (inputs 0..3) and the temperature
//! sensor (input 4), 96 cycles of clk_adc each.
//!
//! One conversion (START_ONCE) or back to back / paced by DIV (START_MANY),
//! with round robin over RROBIN, into RESULT and the 4-entry FIFO (SHIFT,
//! THRESH, OVER/UNDER, the FIFO interrupt). An input reads the voltage set
//! with [`SharedAnalog`], or else what is on the pin (0 or 3.3 V); the
//! temperature sensor follows the datasheet's 0.706 V at 27 °C, -1.721
//! mV/°C. The DMA request (DREQ_EN) is not modelled (noted when used).

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use emu_core::{Clock, DeviceCtx};

use crate::pins::SharedPins;
use crate::regs::{Block, Notices, Op};
use crate::sys::{CLK_ADC, SharedClockTree};

pub const ADC_IRQ_FIFO: u32 = 22;
const CS_EN: u32 = 1 << 0;
const CS_TS_EN: u32 = 1 << 1;
const CS_START_ONCE: u32 = 1 << 2;
const CS_START_MANY: u32 = 1 << 3;
const CS_READY: u32 = 1 << 8;
const FCS_EN: u32 = 1 << 0;
const FCS_SHIFT: u32 = 1 << 1;
const FCS_DREQ_EN: u32 = 1 << 3;
const FCS_UNDER: u32 = 1 << 10;
const FCS_OVER: u32 = 1 << 11;
const VREF: f32 = 3.3;
const CONVERSION_CYCLES: u64 = 96;

/// Voltages at the ADC inputs 0..3 (GPIO 26..29; `None`: the pin's level)
/// and the temperature at the sensor (°C).
#[derive(Clone, Copy, Debug)]
pub struct Analog {
    pub volts: [Option<f32>; 4],
    pub celsius: f32,
}

impl Default for Analog {
    fn default() -> Self {
        Self { volts: [None; 4], celsius: 27.0 }
    }
}

pub type SharedAnalog = Rc<Cell<Analog>>;

pub struct Adc {
    pins: SharedPins,
    tree: SharedClockTree,
    analog: SharedAnalog,
    notices: Notices,
    noted: bool,
    cs: u32,
    result: u32,
    fcs: u32,
    fifo: VecDeque<u16>,
    div: u32,
    inte: u32,
    intf: u32,
    /// End of the next conversion (picoseconds, and the cycle for the
    /// interrupt), and in START_MANY mode the time from one start to the next.
    end_ps: Option<u64>,
    end_cycle: u64,
    interval_ps: u64,
}

impl Adc {
    pub fn new(pins: SharedPins, tree: SharedClockTree, analog: SharedAnalog, notices: Notices) -> Self {
        Self {
            pins,
            tree,
            analog,
            notices,
            noted: false,
            cs: 0,
            result: 0,
            fcs: 0,
            fifo: VecDeque::new(),
            div: 0,
            inte: 0,
            intf: 0,
            end_ps: None,
            end_cycle: 0,
            interval_ps: 0,
        }
    }

    /// Picoseconds of `cycles` cycles of clk_adc (48 MHz if it is stopped:
    /// a conversion then does not end on the chip, but firmware waiting for
    /// it would hang).
    fn adc_ps(&self, cycles: f64) -> u64 {
        let hz = self.tree.borrow().hz(CLK_ADC);
        let hz = if hz == 0 { 48_000_000 } else { hz };
        (cycles * 1e12 / f64::from(hz)) as u64
    }

    fn start(&mut self, clock: &Clock) {
        let now_ps = clock.time_ps();
        let pace = f64::from((self.div >> 8) & 0xFFFF) + f64::from(self.div & 0xFF) / 256.0 + 1.0;
        let between = if self.div == 0 { CONVERSION_CYCLES as f64 } else { pace.max(CONVERSION_CYCLES as f64) };
        self.interval_ps = self.adc_ps(between);
        self.end_ps = Some(now_ps + self.adc_ps(CONVERSION_CYCLES as f64));
        self.end_cycle = clock.cycle_at_ps(now_ps + self.adc_ps(CONVERSION_CYCLES as f64));
    }

    /// Converting now (READY = 0): in START_MANY with DIV, the ADC waits
    /// between conversions.
    fn converting(&self, now_ps: u64) -> bool {
        self.end_ps.is_some_and(|end| end.saturating_sub(self.adc_ps(CONVERSION_CYCLES as f64)) <= now_ps)
    }

    /// The conversion of the selected input: 12 bits.
    fn sample(&self) -> u16 {
        let input = ((self.cs >> 12) & 7) as usize;
        let a = self.analog.get();
        let volts = match input {
            0..=3 => a.volts[input].unwrap_or_else(|| if self.pins.borrow().level(26 + input) { VREF } else { 0.0 }),
            4 if self.cs & CS_TS_EN != 0 => 0.706 - (a.celsius - 27.0) * 0.001_721,
            _ => 0.0,
        };
        (volts / VREF * 4096.0).clamp(0.0, 4095.0) as u16
    }

    /// Next input of the round robin after the current one.
    fn next_input(&mut self) {
        let rrobin = (self.cs >> 16) & 0x1F;
        if rrobin == 0 {
            return;
        }
        let cur = (self.cs >> 12) & 7;
        let next = (1..=5).map(|k| (cur + k) % 5).find(|&i| rrobin & (1 << i) != 0).unwrap_or(cur);
        self.cs = (self.cs & !(7 << 12)) | next << 12;
    }

    /// Completes the conversions that ended by `now_ps`.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now_ps = ctx.clock.time_ps();
        let mut changed = false;
        while let Some(end) = self.end_ps
            && end <= now_ps
        {
            let value = self.sample();
            self.result = u32::from(value);
            if self.fcs & FCS_EN != 0 {
                if self.fifo.len() < 4 {
                    self.fifo.push_back(if self.fcs & FCS_SHIFT != 0 { value >> 4 } else { value });
                } else {
                    self.fcs |= FCS_OVER;
                }
            }
            self.next_input();
            changed = true;
            self.end_ps = (self.cs & CS_START_MANY != 0 && self.cs & CS_EN != 0)
                .then(|| end + self.interval_ps.max(self.adc_ps(CONVERSION_CYCLES as f64)));
            // a whole FIFO full of the same: the rest of a long gap at once
            if let Some(next) = self.end_ps
                && self.interval_ps > 0
                && now_ps > next + 8 * self.interval_ps
            {
                if self.fcs & FCS_EN != 0 && self.fifo.len() == 4 {
                    self.fcs |= FCS_OVER;
                }
                self.end_ps = Some(next + (now_ps - next) / self.interval_ps * self.interval_ps);
            }
        }
        if changed {
            if let Some(end) = self.end_ps {
                self.end_cycle = ctx.clock.cycle_at_ps(end);
            }
            self.sync_irq(ctx);
        }
    }

    fn intr(&self) -> u32 {
        let thresh = ((self.fcs >> 24) & 0xF) as usize;
        u32::from(self.fifo.len() >= thresh.max(1) && self.fcs & FCS_EN != 0)
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        ctx.set_irq(ADC_IRQ_FIFO, (self.intr() | self.intf) & self.inte != 0);
    }

    fn fcs(&self) -> u32 {
        let level = self.fifo.len() as u32;
        (self.fcs & !(0xF << 16 | 3 << 8)) | level << 16 | u32::from(level == 0) << 8 | u32::from(level == 4) << 9
    }
}

impl Block for Adc {
    fn name(&self) -> &str {
        "ADC"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32 {
        self.advance(ctx);
        match off {
            0x00 => {
                let ready = self.cs & CS_EN != 0 && !self.converting(ctx.clock.time_ps());
                self.cs | u32::from(ready) << 8
            }
            0x04 => self.result,
            0x08 => self.fcs(),
            0x0C => {
                let v = match self.fifo.pop_front() {
                    Some(v) => u32::from(v),
                    None => {
                        self.fcs |= FCS_UNDER;
                        0
                    }
                };
                self.sync_irq(ctx);
                v
            }
            0x10 => self.div,
            0x14 => self.intr(),
            0x18 => self.inte,
            0x1C => self.intf,
            0x20 => (self.intr() | self.intf) & self.inte,
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        self.advance(ctx);
        match off {
            0x00 => {
                let v = op.apply(self.cs, value);
                let many_before = self.cs & CS_START_MANY != 0;
                self.cs = v & 0x001F_740B & !(CS_START_ONCE | CS_READY);
                if self.cs & CS_EN == 0 {
                    self.end_ps = None;
                } else if self.end_ps.is_none() && (v & CS_START_ONCE != 0 || !many_before && v & CS_START_MANY != 0) {
                    self.start(ctx.clock);
                }
            }
            0x08 => {
                let v = op.apply(self.fcs, value);
                self.fcs = (v & 0x0F00_000F) | (self.fcs & !op.clears(value) & (FCS_UNDER | FCS_OVER));
                if self.fcs & FCS_DREQ_EN != 0 && !self.noted {
                    self.noted = true;
                    self.notices.note("The firmware uses the ADC's DMA request, which is not emulated".to_owned());
                }
            }
            0x10 => self.div = op.apply(self.div, value) & 0x00FF_FFFF,
            0x18 => self.inte = op.apply(self.inte, value) & 1,
            0x1C => self.intf = op.apply(self.intf, value) & 1,
            _ => {}
        }
        self.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.advance(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        // only the FIFO interrupt needs the conversions on time
        self.end_ps.filter(|_| self.inte != 0 && self.fcs & FCS_EN != 0).map(|_| self.end_cycle)
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        match (off, self.end_ps) {
            (0x0C, _) => clock.cycles(), // pops the FIFO
            (0x00 | 0x04 | 0x08 | 0x14 | 0x20, Some(ps)) => {
                // READY falls at the start of a paced conversion
                let start = ps.saturating_sub(self.adc_ps(CONVERSION_CYCLES as f64));
                clock.cycle_at_ps(if start > clock.time_ps() { start } else { ps })
            }
            _ => u64::MAX,
        }
    }

    fn reset(&mut self) {
        let (pins, tree, analog, notices) =
            (self.pins.clone(), self.tree.clone(), self.analog.clone(), self.notices.clone());
        *self = Self::new(pins, tree, analog, notices);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::nvic::Nvic;

    #[test]
    fn start_many_round_robin_into_the_fifo() {
        let analog = SharedAnalog::default();
        analog.set(Analog { volts: [Some(1.0), Some(2.0), None, None], celsius: 27.0 });
        let mut adc = Adc::new(SharedPins::default(), SharedClockTree::default(), analog, Notices::default());
        let mut clock = Clock::new(48_000_000); // a cycle of clk_adc (stopped: 48 MHz) each
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        adc.write(&mut ctx, 0x08, Op::Write, FCS_EN | 2 << 24); // FIFO, THRESH 2
        adc.write(&mut ctx, 0x18, Op::Write, 1);
        adc.write(&mut ctx, 0x00, Op::Write, CS_EN | CS_START_MANY | 0b11 << 16);
        assert_eq!(adc.next_event(), Some(96));
        ctx.clock.advance(96);
        adc.update(&mut ctx);
        assert_eq!(adc.read(&mut ctx, 0x08) >> 16 & 0xF, 1);
        assert!(!ctx.nvic.is_pending(16 + ADC_IRQ_FIFO), "below the threshold");
        ctx.clock.advance(96);
        adc.update(&mut ctx);
        assert!(ctx.nvic.is_pending(16 + ADC_IRQ_FIFO));
        let (a, b) = (adc.read(&mut ctx, 0x0C), adc.read(&mut ctx, 0x0C));
        assert_eq!((a, b), (1241, 2482), "input 0, then 1");
        // the FIFO overflows while nobody reads it
        ctx.clock.advance(96 * 10);
        assert_ne!(adc.read(&mut ctx, 0x08) & FCS_OVER, 0);
        adc.write(&mut ctx, 0x08, Op::Write, FCS_EN | FCS_OVER);
        assert_eq!(adc.read(&mut ctx, 0x08) & FCS_OVER, 0, "write 1 to clear");
        assert_eq!(adc.read(&mut ctx, 0x00) & CS_READY, 0, "converting");
    }
}
