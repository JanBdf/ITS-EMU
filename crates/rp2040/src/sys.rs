//! Resets, clock generation (XOSC, ROSC, PLLs, clock dividers, frequency
//! counter) and the watchdog's tick. Oscillators and PLLs are stable and
//! locked as soon as they are enabled; the core runs at the frequency the
//! clock registers select.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::{Clock, DeviceCtx};

use crate::regs::{Block, Op};

/// Crystal of the Pico.
pub const XOSC_HZ: u32 = 12_000_000;
/// Ring oscillator (nominal; it varies from chip to chip).
pub const ROSC_HZ: u32 = 6_500_000;

/// Clock generators: index into [`ClockTree::clk`].
pub const CLK_REF: usize = 4;
pub const CLK_SYS: usize = 5;
pub const CLK_PERI: usize = 6;
pub const CLK_USB: usize = 7;
pub const CLK_ADC: usize = 8;
pub const CLK_RTC: usize = 9;

#[derive(Clone, Copy, Debug)]
pub struct PllRegs {
    pub cs: u32,
    pub pwr: u32,
    pub fbdiv: u32,
    pub prim: u32,
}

impl Default for PllRegs {
    fn default() -> Self {
        Self { cs: 1, pwr: 0x2D, fbdiv: 0, prim: 0x0007_7000 }
    }
}

impl PllRegs {
    fn powered(&self) -> bool {
        self.pwr & ((1 << 0) | (1 << 5)) == 0
    }

    pub fn hz(&self) -> u32 {
        if self.cs & (1 << 8) != 0 {
            return XOSC_HZ; // bypass
        }
        if !self.powered() || self.pwr & (1 << 3) != 0 {
            return 0;
        }
        let refdiv = u64::from((self.cs & 0x3F).max(1));
        let pd1 = u64::from(((self.prim >> 16) & 7).max(1));
        let pd2 = u64::from(((self.prim >> 12) & 7).max(1));
        (u64::from(XOSC_HZ) / refdiv * u64::from(self.fbdiv & 0xFFF) / (pd1 * pd2)) as u32
    }
}

/// State shared by the clock blocks.
#[derive(Debug)]
pub struct ClockTree {
    /// CTRL and DIV of the ten clock generators (GPOUT0..3, REF, SYS, PERI, USB, ADC, RTC).
    pub clk: [(u32, u32); 10],
    pub pll: [PllRegs; 2],
}

impl Default for ClockTree {
    fn default() -> Self {
        Self { clk: [(0, 0x100); 10], pll: [PllRegs::default(); 2] }
    }
}

pub type SharedClockTree = Rc<RefCell<ClockTree>>;

impl ClockTree {
    fn pll_sys(&self) -> u32 {
        self.pll[0].hz()
    }

    fn pll_usb(&self) -> u32 {
        self.pll[1].hz()
    }

    fn integer_div(hz: u32, div: u32) -> u32 {
        let d = (div >> 8) & 0x3;
        hz.checked_div(d).unwrap_or(hz / 4)
    }

    fn fractional_div(hz: u32, div: u32) -> u32 {
        let d = if div == 0 { 1 << 32 } else { u64::from(div) };
        (u64::from(hz) * 256 / d) as u32
    }

    /// Frequency of clock generator `i` (0 when stopped).
    pub fn hz(&self, i: usize) -> u32 {
        let (ctrl, div) = self.clk[i];
        let aux = (ctrl >> 5) & 0xF;
        match i {
            CLK_REF => {
                let src = match ctrl & 3 {
                    0 => ROSC_HZ,
                    1 => match aux & 3 {
                        0 => self.pll_usb(),
                        _ => 0,
                    },
                    _ => XOSC_HZ,
                };
                Self::integer_div(src, div)
            }
            CLK_SYS => {
                let src = if ctrl & 1 == 0 {
                    self.hz(CLK_REF)
                } else {
                    match aux & 7 {
                        0 => self.pll_sys(),
                        1 => self.pll_usb(),
                        2 => ROSC_HZ,
                        3 => XOSC_HZ,
                        _ => 0,
                    }
                };
                Self::fractional_div(src, div)
            }
            _ if ctrl & (1 << 11) == 0 => 0, // ENABLE
            CLK_PERI => match aux & 7 {
                0 => self.hz(CLK_SYS),
                1 => self.pll_sys(),
                2 => self.pll_usb(),
                3 => ROSC_HZ,
                4 => XOSC_HZ,
                _ => 0,
            },
            CLK_USB | CLK_ADC | CLK_RTC => {
                let src = match aux & 7 {
                    0 => self.pll_usb(),
                    1 => self.pll_sys(),
                    2 => ROSC_HZ,
                    3 => XOSC_HZ,
                    _ => 0,
                };
                if i == CLK_RTC { Self::fractional_div(src, div) } else { Self::integer_div(src, div) }
            }
            _ => 0,
        }
    }

    /// The core follows clk_sys.
    fn sync_core(&self, clock: &mut Clock) {
        let hz = self.hz(CLK_SYS);
        if hz > 0 {
            clock.set_cpu_hz(hz);
        }
    }
}

/// CLOCKS block: clock generators and the frequency counter.
pub struct Clocks {
    tree: SharedClockTree,
    regs: [u32; 0xC8 / 4],
    fc0_status: u32,
    fc0_result: u32,
}

impl Clocks {
    pub fn new(tree: SharedClockTree) -> Self {
        Self { tree, regs: [0; 0xC8 / 4], fc0_status: 0, fc0_result: 0 }
    }

    fn measure(&self, src: u32) -> u32 {
        let t = self.tree.borrow();
        match src {
            1 => t.pll_sys(),
            2 => t.pll_usb(),
            3 | 4 => ROSC_HZ,
            5 => XOSC_HZ,
            8 => t.hz(CLK_REF),
            9 => t.hz(CLK_SYS),
            10 => t.hz(CLK_PERI),
            11 => t.hz(CLK_USB),
            12 => t.hz(CLK_ADC),
            13 => t.hz(CLK_RTC),
            _ => 0,
        }
    }
}

impl Block for Clocks {
    fn name(&self) -> &str {
        "CLOCKS"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        match off {
            0x00..=0x74 => {
                let (i, reg) = ((off / 12) as usize, off % 12);
                let (ctrl, div) = self.tree.borrow().clk[i];
                match reg {
                    0 => ctrl,
                    4 => div,
                    // Only REF and SYS have glitchless muxes: one bit per source.
                    _ if i == CLK_REF => 1 << (ctrl & 3),
                    _ if i == CLK_SYS => 1 << (ctrl & 1),
                    _ => 1,
                }
            }
            0x98 => self.fc0_status,
            0x9C => self.fc0_result,
            0xB0 | 0xB4 => u32::MAX, // ENABLED0/1
            _ => self.regs[(off / 4) as usize],
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        match off {
            0x00..=0x74 => {
                let (i, reg) = ((off / 12) as usize, off % 12);
                let mut t = self.tree.borrow_mut();
                match reg {
                    0 => t.clk[i].0 = op.apply(t.clk[i].0, value),
                    4 => t.clk[i].1 = op.apply(t.clk[i].1, value),
                    _ => {}
                }
                t.sync_core(ctx.clock);
            }
            0x94 => {
                // FC0_SRC: the count is done at once
                let src = op.apply(0, value) & 0xFF;
                self.regs[(off / 4) as usize] = src;
                if src != 0 {
                    let hz = self.measure(src);
                    let khz_32 = (u64::from(hz) * 32 / 1000) as u32;
                    self.fc0_result = khz_32 & 0x3FFF_FFFF;
                    self.fc0_status = (1 << 4) | 1; // DONE, PASS
                }
            }
            _ => {
                if let Some(r) = self.regs.get_mut((off / 4) as usize) {
                    *r = op.apply(*r, value);
                }
            }
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        *self.tree.borrow_mut() = ClockTree::default();
        self.regs = [0; 0xC8 / 4];
        self.fc0_status = 0;
        self.fc0_result = 0;
    }
}

/// PLL_SYS (`index` 0) or PLL_USB (1): locks at once.
pub struct Pll {
    tree: SharedClockTree,
    index: usize,
}

impl Pll {
    pub fn new(tree: SharedClockTree, index: usize) -> Self {
        Self { tree, index }
    }
}

impl Block for Pll {
    fn name(&self) -> &str {
        if self.index == 0 { "PLL_SYS" } else { "PLL_USB" }
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        let p = self.tree.borrow().pll[self.index];
        match off {
            0x0 => (p.cs & !(1 << 31)) | if p.powered() { 1 << 31 } else { 0 },
            0x4 => p.pwr,
            0x8 => p.fbdiv,
            0xC => p.prim,
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        let mut t = self.tree.borrow_mut();
        let p = &mut t.pll[self.index];
        match off {
            0x0 => p.cs = op.apply(p.cs, value) & 0x13F,
            0x4 => p.pwr = op.apply(p.pwr, value) & 0x2D,
            0x8 => p.fbdiv = op.apply(p.fbdiv, value) & 0xFFF,
            0xC => p.prim = op.apply(p.prim, value) & 0x0007_7000,
            _ => {}
        }
        t.sync_core(ctx.clock);
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.tree.borrow_mut().pll[self.index] = PllRegs::default();
    }
}

/// XOSC and ROSC: registers kept, STATUS reports a stable oscillator.
pub struct Oscillator {
    name: &'static str,
    status_off: u32,
    regs: [u32; 0x24 / 4],
    random: u32,
}

impl Oscillator {
    pub fn xosc() -> Self {
        Self { name: "XOSC", status_off: 0x04, regs: [0; 9], random: 0 }
    }

    pub fn rosc() -> Self {
        Self { name: "ROSC", status_off: 0x18, regs: [0; 9], random: 0xACE1 }
    }
}

impl Block for Oscillator {
    fn name(&self) -> &str {
        self.name
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        if off == self.status_off {
            return (1 << 31) | (1 << 12); // STABLE, ENABLED
        }
        match (self.name, off) {
            ("ROSC", 0x1C) => {
                // RANDOMBIT: a 16-bit LFSR
                let bit = (self.random ^ (self.random >> 2) ^ (self.random >> 3) ^ (self.random >> 5)) & 1;
                self.random = (self.random >> 1) | (bit << 15);
                bit
            }
            // COUNT: counts down at once
            ("XOSC", 0x1C) | ("ROSC", 0x20) => 0,
            _ => self.regs.get((off / 4) as usize).copied().unwrap_or(0),
        }
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = op.apply(*r, value);
        }
    }

    fn reset(&mut self) {
        self.regs = [0; 9];
    }
}

/// RESETS: peripherals come out of reset as soon as they are released.
pub struct Resets {
    reset: u32,
    wdsel: u32,
}

const RESETS_ALL: u32 = 0x01FF_FFFF;

impl Resets {
    pub fn new() -> Self {
        Self { reset: RESETS_ALL, wdsel: 0 }
    }
}

impl Default for Resets {
    fn default() -> Self {
        Self::new()
    }
}

impl Block for Resets {
    fn name(&self) -> &str {
        "RESETS"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        match off {
            0x0 => self.reset,
            0x4 => self.wdsel,
            0x8 => !self.reset & RESETS_ALL,
            _ => 0,
        }
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        match off {
            0x0 => self.reset = op.apply(self.reset, value) & RESETS_ALL,
            0x4 => self.wdsel = op.apply(self.wdsel, value) & RESETS_ALL,
            _ => {}
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

/// WATCHDOG: scratch registers and the tick generator for the timer. The
/// watchdog counter itself is not modelled (a trigger is logged).
pub struct Watchdog {
    regs: [u32; 0x30 / 4],
}

impl Watchdog {
    pub fn new() -> Self {
        Self { regs: [0; 12] }
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl Block for Watchdog {
    fn name(&self) -> &str {
        "WATCHDOG"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        let v = self.regs.get((off / 4) as usize).copied().unwrap_or(0);
        match off {
            // TICK: RUNNING follows ENABLE
            0x2C if v & (1 << 9) != 0 => v | (1 << 10),
            _ => v,
        }
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = op.apply(*r, value);
        }
        if off == 0 && self.regs[0] & (1 << 31) != 0 {
            log::warn!("Pico: watchdog trigger (reset) is not modelled");
            self.regs[0] &= !(1 << 31);
        }
        if off == 0 && self.regs[0] & (1 << 30) != 0 {
            log::debug!("Pico: watchdog enabled (its timeout is not modelled)");
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        // Scratch registers survive a reset of the chip (except power-on).
        self.regs[0] = 0;
        self.regs[11] = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_default_clocks() {
        let mut t = ClockTree::default();
        // pll_init(pll_sys, 1, 1500 MHz, 6, 2); pll_init(pll_usb, 1, 1200 MHz, 5, 5)
        t.pll[0] = PllRegs { cs: 1, pwr: 0, fbdiv: 125, prim: (6 << 16) | (2 << 12) };
        t.pll[1] = PllRegs { cs: 1, pwr: 0, fbdiv: 100, prim: (5 << 16) | (5 << 12) };
        assert_eq!(t.pll_sys(), 125_000_000);
        assert_eq!(t.pll_usb(), 48_000_000);
        t.clk[CLK_REF] = (2, 1 << 8); // XOSC / 1
        t.clk[CLK_SYS] = (1, 1 << 8); // aux = pll_sys, / 1
        assert_eq!(t.hz(CLK_REF), 12_000_000);
        assert_eq!(t.hz(CLK_SYS), 125_000_000);
        t.clk[CLK_PERI] = (1 << 11, 0); // clk_sys
        assert_eq!(t.hz(CLK_PERI), 125_000_000);
    }
}
