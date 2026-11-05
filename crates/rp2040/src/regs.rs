//! Register conventions of the RP2040's APB and AHB-lite peripherals: each
//! 4 KiB block is seen four times, as normal registers and as atomic XOR,
//! set and clear aliases (+0x1000, +0x2000, +0x3000). 8- and 16-bit writes
//! are replicated across the 32-bit word.

use emu_core::bus::extract_read;
use emu_core::{Clock, DeviceCtx, Peripheral};

/// How a write through one of the aliases combines with the register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Write,
    Xor,
    Set,
    Clear,
}

impl Op {
    pub fn apply(self, old: u32, value: u32) -> u32 {
        match self {
            Op::Write => value,
            Op::Xor => old ^ value,
            Op::Set => old | value,
            Op::Clear => old & !value,
        }
    }

    /// For write-1-to-clear registers: the bits a write clears. Writes of
    /// ones through any alias clear them (TinyUSB uses the clear alias).
    pub fn clears(self, value: u32) -> u32 {
        match self {
            Op::Xor => 0,
            _ => value,
        }
    }
}

/// A register block behind the alias decoding of [`Aliased`]. Offsets are
/// word aligned and below 0x1000.
pub trait Block {
    fn name(&self) -> &str;
    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32;
    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32);
    fn update(&mut self, _ctx: &mut DeviceCtx) {}
    fn next_event(&self) -> Option<u64> {
        None
    }
    fn read_stable_until(&self, _off: u32, clock: &Clock) -> u64 {
        clock.cycles()
    }
    fn reset(&mut self) {}
}

/// A [`Block`] as a 16 KiB peripheral window with its aliases.
pub struct Aliased<B>(pub B);

pub const BLOCK_WINDOW: u32 = 0x4000;

fn decode(off: u32) -> (u32, Op) {
    let op = match (off >> 12) & 3 {
        0 => Op::Write,
        1 => Op::Xor,
        2 => Op::Set,
        _ => Op::Clear,
    };
    (off & 0xFFC, op)
}

impl<B: Block> Peripheral for Aliased<B> {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        let (reg, _) = decode(off);
        extract_read(self.0.read(ctx, reg), off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        let (reg, op) = decode(off);
        let value = match size {
            1 => (value & 0xFF) * 0x0101_0101,
            2 => (value & 0xFFFF) * 0x0001_0001,
            _ => value,
        };
        self.0.write(ctx, reg, op, value);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.0.update(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        self.0.next_event()
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        self.0.read_stable_until(off & 0xFFC, clock)
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

/// Plain registers that only store what is written (blocks the firmware
/// configures but whose function is not modelled), with reset values and
/// read-only values.
pub struct RegFile {
    name: &'static str,
    regs: Vec<u32>,
    resets: &'static [(u32, u32)],
    fixed: &'static [(u32, u32)],
}

impl RegFile {
    /// `resets`: (offset, reset value); `fixed`: (offset, value it always reads).
    pub fn new(name: &'static str, size: u32, resets: &'static [(u32, u32)], fixed: &'static [(u32, u32)]) -> Self {
        let mut r = Self { name, regs: vec![0; (size / 4) as usize], resets, fixed };
        r.reset();
        r
    }
}

impl Block for RegFile {
    fn name(&self) -> &str {
        self.name
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        if let Some(&(_, v)) = self.fixed.iter().find(|&&(o, _)| o == off) {
            return v;
        }
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = op.apply(*r, value);
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.regs.fill(0);
        for &(off, v) in self.resets {
            if let Some(r) = self.regs.get_mut((off / 4) as usize) {
                *r = v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_set_clear_and_xor() {
        assert_eq!(Op::Set.apply(0b0101, 0b0011), 0b0111);
        assert_eq!(Op::Clear.apply(0b0101, 0b0011), 0b0100);
        assert_eq!(Op::Xor.apply(0b0101, 0b0011), 0b0110);
        assert_eq!(decode(0x2010), (0x010, Op::Set));
        assert_eq!(decode(0x3ffc), (0xffc, Op::Clear));
    }
}
