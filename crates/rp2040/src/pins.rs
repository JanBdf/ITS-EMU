//! GPIO0..29: function select and overrides (IO_BANK0), pads (PADS_BANK0)
//! and what each pin drives. Every change of a pin's output is logged with
//! its time, for the circuitry outside the chip.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::{Clock, DeviceCtx};

use crate::regs::{Block, Op};

pub const NUM_GPIO: usize = 30;
pub const FUNC_PWM: u32 = 4;
pub const FUNC_SIO: u32 = 5;
pub const FUNC_PIO0: u32 = 6;
pub const FUNC_PIO1: u32 = 7;

/// A pin changed what it drives: `Some(level)` or released (`None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinEdge {
    /// Time since the Pico started, in picoseconds.
    pub ps: u64,
    pub pin: u8,
    pub level: Option<bool>,
}

pub struct Pins {
    /// GPIOn_CTRL (function select, overrides).
    pub ctrl: [u32; NUM_GPIO],
    /// PADS_BANK0 GPIOn (pulls, input enable, ...).
    pub pads: [u32; NUM_GPIO],
    pub sio_out: u32,
    pub sio_oe: u32,
    /// Outputs and output enables of PIO0/PIO1 towards the pins.
    pub pio_out: [u32; 2],
    pub pio_oe: [u32; 2],
    /// PWM outputs: bit 2n + ch = channel ch of slice n.
    pub pwm_out: u32,
    /// Levels applied to the pins from outside (`None`: not connected).
    pub external: [Option<bool>; NUM_GPIO],
    driven: [Option<bool>; NUM_GPIO],
    edges: Vec<PinEdge>,
}

pub type SharedPins = Rc<RefCell<Pins>>;

const CTRL_RESET: u32 = 0x1F; // FUNCSEL = NULL
const PAD_RESET: u32 = 0x56; // IE, 4 mA, pull-down, Schmitt trigger

impl Default for Pins {
    fn default() -> Self {
        Self {
            ctrl: [CTRL_RESET; NUM_GPIO],
            pads: [PAD_RESET; NUM_GPIO],
            sio_out: 0,
            sio_oe: 0,
            pio_out: [0; 2],
            pio_oe: [0; 2],
            pwm_out: 0,
            external: [None; NUM_GPIO],
            driven: [None; NUM_GPIO],
            edges: Vec::new(),
        }
    }
}

impl Pins {
    /// Output and output enable that the selected function gives pin `n`,
    /// after the overrides.
    fn peripheral_drive(&self, n: usize) -> (bool, bool) {
        let ctrl = self.ctrl[n];
        let bit = 1 << n;
        let (out, oe) = match ctrl & 0x1F {
            FUNC_SIO => (self.sio_out & bit != 0, self.sio_oe & bit != 0),
            FUNC_PIO0 => (self.pio_out[0] & bit != 0, self.pio_oe[0] & bit != 0),
            FUNC_PIO1 => (self.pio_out[1] & bit != 0, self.pio_oe[1] & bit != 0),
            // GPIO n: slice n / 2 mod 8, channel n mod 2
            FUNC_PWM => (self.pwm_out & (1 << (n % 16)) != 0, true),
            _ => (false, false),
        };
        let out = match (ctrl >> 8) & 3 {
            0 => out,
            1 => !out,
            2 => false,
            _ => true,
        };
        let oe = match (ctrl >> 12) & 3 {
            0 => oe,
            1 => !oe,
            2 => false,
            _ => true,
        };
        (out, oe)
    }

    /// What the Pico drives on pin `n`.
    pub fn drive(&self, n: usize) -> Option<bool> {
        let (out, oe) = self.peripheral_drive(n);
        oe.then_some(out)
    }

    /// Level at pin `n` as the input buffer sees it.
    pub fn level(&self, n: usize) -> bool {
        if let Some(l) = self.drive(n).or(self.external[n]) {
            return l;
        }
        let pad = self.pads[n];
        // pull-up wins over pull-down when both are on (bus keeper: low)
        pad & (1 << 3) != 0
    }

    /// GPIO_IN as SIO reads it (pins with the input buffer disabled read 0).
    pub fn gpio_in(&self) -> u32 {
        (0..NUM_GPIO).filter(|&n| self.pads[n] & (1 << 6) != 0 && self.level(n)).fold(0, |acc, n| acc | (1 << n))
    }

    /// Recomputes what the pins drive and logs changes at `ps`.
    pub fn refresh(&mut self, ps: u64) {
        for n in 0..NUM_GPIO {
            let d = self.drive(n);
            if d != self.driven[n] {
                self.driven[n] = d;
                self.edges.push(PinEdge { ps, pin: n as u8, level: d });
            }
        }
    }

    /// The pin changes since the last call, in time order.
    pub fn take_edges(&mut self) -> Vec<PinEdge> {
        let mut e = std::mem::take(&mut self.edges);
        e.sort_by_key(|e| e.ps);
        e
    }

    /// What each pin drives now.
    pub fn outputs(&self) -> [Option<bool>; NUM_GPIO] {
        self.driven
    }

    pub fn reset(&mut self) {
        let external = self.external;
        *self = Self { external, ..Self::default() };
    }
}

/// IO_BANK0: GPIOn_STATUS/CTRL and the (unmodelled) GPIO interrupt registers.
pub struct IoBank0 {
    pins: SharedPins,
    irq_regs: [u32; (0x190 - 0xF0) / 4],
}

impl IoBank0 {
    pub fn new(pins: SharedPins) -> Self {
        Self { pins, irq_regs: [0; (0x190 - 0xF0) / 4] }
    }
}

impl Block for IoBank0 {
    fn name(&self) -> &str {
        "IO_BANK0"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        let p = self.pins.borrow();
        match off {
            0x00..=0xEC => {
                let n = (off / 8) as usize;
                if off % 8 == 4 {
                    return p.ctrl[n];
                }
                let (out, oe) = p.peripheral_drive(n);
                let level = p.level(n);
                u32::from(out) << 8
                    | u32::from(out) << 9
                    | u32::from(oe) << 12
                    | u32::from(oe) << 13
                    | u32::from(level) << 17
                    | u32::from(level) << 19
            }
            _ => self.irq_regs.get(((off - 0xF0) / 4) as usize).copied().unwrap_or(0),
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        match off {
            0x00..=0xEC if off % 8 == 4 => {
                let mut p = self.pins.borrow_mut();
                let n = (off / 8) as usize;
                p.ctrl[n] = op.apply(p.ctrl[n], value) & 0x3003_331F;
                p.refresh(ctx.clock.time_ps());
            }
            0xF0.. => {
                if let Some(r) = self.irq_regs.get_mut(((off - 0xF0) / 4) as usize) {
                    let v = op.apply(*r, value);
                    if v != 0 && (0x100..0x130).contains(&off) {
                        log::warn!("Pico: GPIO interrupts are not modelled (IO_BANK0 +{off:#x} = {v:#x})");
                    }
                    *r = v;
                }
            }
            _ => {}
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.irq_regs.fill(0);
        self.pins.borrow_mut().ctrl = [CTRL_RESET; NUM_GPIO];
    }
}

/// PADS_BANK0: pad controls of GPIO0..29 (+ SWCLK, SWD).
pub struct PadsBank0 {
    pins: SharedPins,
    other: [u32; 3],
}

impl PadsBank0 {
    pub fn new(pins: SharedPins) -> Self {
        Self { pins, other: [0, 0xDA, 0x5A] }
    }
}

impl Block for PadsBank0 {
    fn name(&self) -> &str {
        "PADS_BANK0"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        match off {
            0x0 => self.other[0],
            0x04..=0x78 => self.pins.borrow().pads[((off - 4) / 4) as usize],
            0x7C => self.other[1],
            0x80 => self.other[2],
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        match off {
            0x0 => self.other[0] = op.apply(self.other[0], value) & 1,
            0x04..=0x78 => {
                let mut p = self.pins.borrow_mut();
                let n = ((off - 4) / 4) as usize;
                p.pads[n] = op.apply(p.pads[n], value) & 0xFF;
                // Output disable (OD) is not modelled; pulls only matter for inputs.
                p.refresh(ctx.clock.time_ps());
            }
            0x7C => self.other[1] = op.apply(self.other[1], value) & 0xFF,
            0x80 => self.other[2] = op.apply(self.other[2], value) & 0xFF,
            _ => {}
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.other = [0, 0xDA, 0x5A];
        self.pins.borrow_mut().pads = [PAD_RESET; NUM_GPIO];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_select_and_overrides() {
        let mut p = Pins { sio_out: 1 << 3, sio_oe: 1 << 3, ..Default::default() };
        assert_eq!(p.drive(3), None, "not connected to SIO yet");
        p.ctrl[3] = FUNC_SIO;
        assert_eq!(p.drive(3), Some(true));
        p.ctrl[3] = FUNC_SIO | (1 << 8); // invert output
        assert_eq!(p.drive(3), Some(false));
        p.refresh(5);
        assert_eq!(p.take_edges(), vec![PinEdge { ps: 5, pin: 3, level: Some(false) }]);
        // undriven inputs read their pull-down
        assert!(!p.level(4));
        p.pads[4] |= 1 << 3;
        assert!(p.level(4));
    }
}
