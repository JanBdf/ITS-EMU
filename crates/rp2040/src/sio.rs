//! SIO, one per core: CPUID, GPIO registers, the core's hardware divider,
//! and the spinlocks and inter-core FIFOs both cores share. The
//! interpolators keep their registers but do not compute.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::pins::SharedPins;

/// Each core's FIFO interrupt (RX data, or an error flag).
pub const SIO_IRQ_PROC0: u32 = 15;
pub const SIO_IRQ_PROC1: u32 = 16;
const FIFO_DEPTH: usize = 8;

/// What the two cores' SIOs share.
#[derive(Default)]
pub struct SioShared {
    /// `fifo[n]`: written by core n, read by the other.
    fifo: [VecDeque<u32>; 2],
    /// WOF / ROE flags (FIFO_ST bits 2, 3) of each core.
    sticky: [u32; 2],
    spinlocks: u32,
}

pub type SharedSio = Rc<RefCell<SioShared>>;

pub struct Sio {
    core: usize,
    shared: SharedSio,
    pins: SharedPins,
    // divider
    dividend: u32,
    divisor: u32,
    quotient: u32,
    remainder: u32,
    dirty: bool,
    interp: [u32; 32],
    warned_interp: bool,
}

impl Sio {
    /// The SIO of core `core` (0 or 1).
    pub fn new(core: usize, shared: SharedSio, pins: SharedPins) -> Self {
        Self {
            core,
            shared,
            pins,
            dividend: 0,
            divisor: 0,
            quotient: 0,
            remainder: 0,
            dirty: false,
            interp: [0; 32],
            warned_interp: false,
        }
    }

    fn divide(&mut self, signed: bool) {
        let (n, d) = (self.dividend, self.divisor);
        (self.quotient, self.remainder) = if signed {
            let (n, d) = (n as i32, d as i32);
            if d == 0 {
                (if n < 0 { 1 } else { u32::MAX }, n as u32)
            } else {
                (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32)
            }
        } else {
            n.checked_div(d).map_or((u32::MAX, n), |q| (q, n % d))
        };
        self.dirty = true;
    }

    fn fifo_status(&self) -> u32 {
        let s = self.shared.borrow();
        let vld = !s.fifo[1 - self.core].is_empty();
        let rdy = s.fifo[self.core].len() < FIFO_DEPTH;
        u32::from(vld) | (u32::from(rdy) << 1) | s.sticky[self.core]
    }

    /// SIO_IRQ_PROCn follows FIFO_ST's VLD, WOF and ROE.
    fn update_irq(&self, ctx: &mut DeviceCtx) {
        let irq = if self.core == 0 { SIO_IRQ_PROC0 } else { SIO_IRQ_PROC1 };
        ctx.set_irq(irq, self.fifo_status() & 0b1101 != 0);
    }

    fn gpio_write(&mut self, ctx: &DeviceCtx, f: impl FnOnce(&mut crate::pins::Pins)) {
        let mut p = self.pins.borrow_mut();
        f(&mut p);
        p.sio_out &= 0x3FFF_FFFF;
        p.sio_oe &= 0x3FFF_FFFF;
        p.refresh(ctx.clock.time_ps());
    }
}

impl Peripheral for Sio {
    fn name(&self) -> &str {
        "SIO"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, _size: u32) -> u32 {
        match off & !3 {
            0x000 => self.core as u32,
            0x004 => self.pins.borrow().gpio_in(),
            0x008 => 0x3F, // QSPI pins (CS high, ...)
            0x010 => self.pins.borrow().sio_out,
            0x020 => self.pins.borrow().sio_oe,
            0x050 => self.fifo_status(),
            0x058 => {
                let mut s = self.shared.borrow_mut();
                let value = s.fifo[1 - self.core].pop_front();
                if value.is_none() {
                    s.sticky[self.core] |= 1 << 3; // ROE
                }
                drop(s);
                self.update_irq(ctx);
                value.unwrap_or(0)
            }
            0x05C => self.shared.borrow().spinlocks,
            0x060 | 0x068 => self.dividend,
            0x064 | 0x06C => self.divisor,
            0x070 => {
                self.dirty = false;
                self.quotient
            }
            0x074 => self.remainder,
            0x078 => 1 | (u32::from(self.dirty) << 1), // READY, DIRTY
            o @ 0x080..=0x0FC => {
                if !self.warned_interp {
                    self.warned_interp = true;
                    log::warn!("Pico: interpolators are not modelled");
                }
                self.interp[((o - 0x80) / 4) as usize]
            }
            o @ 0x100..=0x17C => {
                let bit = 1 << ((o - 0x100) / 4);
                let mut s = self.shared.borrow_mut();
                if s.spinlocks & bit != 0 {
                    0
                } else {
                    s.spinlocks |= bit;
                    bit
                }
            }
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, _size: u32, value: u32) {
        match off & !3 {
            0x010 => self.gpio_write(ctx, |p| p.sio_out = value),
            0x014 => self.gpio_write(ctx, |p| p.sio_out |= value),
            0x018 => self.gpio_write(ctx, |p| p.sio_out &= !value),
            0x01C => self.gpio_write(ctx, |p| p.sio_out ^= value),
            0x020 => self.gpio_write(ctx, |p| p.sio_oe = value),
            0x024 => self.gpio_write(ctx, |p| p.sio_oe |= value),
            0x028 => self.gpio_write(ctx, |p| p.sio_oe &= !value),
            0x02C => self.gpio_write(ctx, |p| p.sio_oe ^= value),
            0x050 => {
                self.shared.borrow_mut().sticky[self.core] &= !(value & 0b1100); // clear WOF/ROE
                self.update_irq(ctx);
            }
            0x054 => {
                let mut s = self.shared.borrow_mut();
                if s.fifo[self.core].len() < FIFO_DEPTH {
                    s.fifo[self.core].push_back(value);
                } else {
                    s.sticky[self.core] |= 1 << 2; // WOF
                }
                drop(s);
                self.update_irq(ctx);
            }
            0x060 => {
                self.dividend = value;
                self.divide(false);
            }
            0x064 => {
                self.divisor = value;
                self.divide(false);
            }
            0x068 => {
                self.dividend = value;
                self.divide(true);
            }
            0x06C => {
                self.divisor = value;
                self.divide(true);
            }
            0x070 => {
                self.quotient = value;
                self.dirty = true;
            }
            0x074 => {
                self.remainder = value;
                self.dirty = true;
            }
            o @ 0x080..=0x0FC => self.interp[((o - 0x80) / 4) as usize] = value,
            o @ 0x100..=0x17C => self.shared.borrow_mut().spinlocks &= !(1 << ((o - 0x100) / 4)),
            _ => {}
        }
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        // the other core may have written to our RX FIFO
        self.update_irq(ctx);
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        match off & !3 {
            // the PIOs and the board drive the pins
            0x004 => clock.cycles(),
            // spinlocks change on reads, the divider's DIRTY too
            0x070 | 0x100..=0x17C => 0,
            // the FIFOs and spinlocks change when the other core runs, which
            // is after this core's turn (that idle skipping does not pass)
            _ => u64::MAX,
        }
    }

    fn reset(&mut self) {
        *self.shared.borrow_mut() = SioShared::default();
        *self = Self::new(self.core, self.shared.clone(), self.pins.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::nvic::Nvic;

    #[test]
    fn divider_matches_the_datasheet() {
        let mut s = Sio::new(0, SharedSio::default(), SharedPins::default());
        let mut clock = Clock::new(125_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        s.write(&mut ctx, 0x68, 4, (-7i32) as u32);
        s.write(&mut ctx, 0x6C, 4, 2);
        assert_eq!(s.read(&mut ctx, 0x74, 4) as i32, -1);
        assert_eq!(s.read(&mut ctx, 0x78, 4), 3, "ready and dirty");
        assert_eq!(s.read(&mut ctx, 0x70, 4) as i32, -3);
        assert_eq!(s.read(&mut ctx, 0x78, 4), 1, "reading the quotient clears DIRTY");
        s.write(&mut ctx, 0x60, 4, 100);
        s.write(&mut ctx, 0x64, 4, 0);
        assert_eq!((s.read(&mut ctx, 0x70, 4), s.read(&mut ctx, 0x74, 4)), (u32::MAX, 100));
        // spinlock: the first read claims it, a write releases it
        assert_eq!(s.read(&mut ctx, 0x104, 4), 2);
        assert_eq!(s.read(&mut ctx, 0x104, 4), 0);
        s.write(&mut ctx, 0x104, 4, 0);
        assert_eq!(s.read(&mut ctx, 0x104, 4), 2);
    }

    #[test]
    fn fifos_connect_the_cores() {
        let shared = SharedSio::default();
        let pins = SharedPins::default();
        let mut core0 = Sio::new(0, shared.clone(), pins.clone());
        let mut core1 = Sio::new(1, shared, pins);
        let mut clock = Clock::new(125_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        assert_eq!(core1.read(&mut ctx, 0x000, 4), 1, "CPUID");
        assert_eq!(core1.read(&mut ctx, 0x050, 4), 0b10, "ready, nothing received");
        for v in 0..9 {
            core0.write(&mut ctx, 0x054, 4, v);
        }
        assert_eq!(core0.read(&mut ctx, 0x050, 4), 0b100, "full, write on full");
        assert_eq!(core1.read(&mut ctx, 0x050, 4), 0b11);
        assert_eq!(core1.read(&mut ctx, 0x058, 4), 0);
        assert_eq!(core1.read(&mut ctx, 0x058, 4), 1);
        assert!(ctx.nvic.irq_level(SIO_IRQ_PROC1), "data waiting for core 1");
    }
}
