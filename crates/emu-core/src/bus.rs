//! Memory-mapped peripheral bus.
//!
//! Peripherals register an address window. Accesses to peripheral space that no
//! model claims read as zero and ignore writes (logged once per address), which
//! keeps firmware running when it touches peripherals we do not model yet.

use std::collections::HashSet;

use crate::clock::Clock;
use crate::nvic::Nvic;

/// Context handed to peripherals on every access and update.
pub struct DeviceCtx<'a> {
    pub clock: &'a mut Clock,
    pub nvic: &'a mut Nvic,
}

impl DeviceCtx<'_> {
    pub fn now(&self) -> u64 {
        self.clock.cycles()
    }

    /// Drives the level of external interrupt line `irq`.
    pub fn set_irq(&mut self, irq: u32, high: bool) {
        self.nvic.set_irq_level(irq, high);
    }
}

pub trait Peripheral {
    fn name(&self) -> &str;

    /// Reads `size` bytes (1, 2 or 4) at `offset` within the peripheral window.
    fn read(&mut self, ctx: &mut DeviceCtx, offset: u32, size: u32) -> u32;

    fn write(&mut self, ctx: &mut DeviceCtx, offset: u32, size: u32, value: u32);

    /// Brings time-dependent state up to `ctx.now()`.
    fn update(&mut self, _ctx: &mut DeviceCtx) {}

    /// Until which cycle a read of the register at `offset` keeps returning
    /// the same value, as long as nothing writes to the peripheral and no
    /// external input or scheduled event (see [`Self::next_event`]) happens:
    /// `u64::MAX` for registers that only change on writes/events, the cycle
    /// of the next counter tick for counters, `clock.cycles()` if unknown.
    /// Used to fast-forward idle polling loops. Conservative default: now.
    fn read_stable_until(&self, _offset: u32, clock: &Clock) -> u64 {
        clock.cycles()
    }

    /// Cycle of the next internal event (timer overflow, ...), if any.
    fn next_event(&self) -> Option<u64> {
        None
    }

    fn reset(&mut self) {}
}

struct Mapping {
    base: u32,
    size: u32,
    dev: Box<dyn Peripheral>,
}

/// Extra cycles (bus wait states) of an access to `addr` (`true`: write)
/// that starts at the given cycle.
pub type WaitStates = Box<dyn Fn(u32, bool, u64) -> u64>;

#[derive(Default)]
pub struct Bus {
    maps: Vec<Mapping>,
    warned: HashSet<u32>,
    /// Earliest [`Peripheral::next_event`] of the peripherals accessed since
    /// the last [`Bus::take_access_event`].
    access_event: Option<u64>,
    wait_states: Option<WaitStates>,
}

impl Bus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a peripheral at `[base, base + size)`. Windows must not overlap.
    pub fn attach(&mut self, base: u32, size: u32, dev: Box<dyn Peripheral>) {
        assert!(
            self.maps.iter().all(|m| base + size <= m.base || m.base + m.size <= base),
            "peripheral window of {} overlaps an existing mapping",
            dev.name()
        );
        let pos = self.maps.partition_point(|m| m.base < base);
        self.maps.insert(pos, Mapping { base, size, dev });
    }

    fn find(&mut self, addr: u32) -> Option<&mut Mapping> {
        let idx = self.maps.partition_point(|m| m.base <= addr).checked_sub(1)?;
        let m = &mut self.maps[idx];
        (addr - m.base < m.size).then_some(m)
    }

    pub fn read(&mut self, ctx: &mut DeviceCtx, addr: u32, size: u32) -> u32 {
        if let Some((word, bit)) = bitband_target(addr) {
            return (self.read_aligned(ctx, word, 4) >> bit) & 1;
        }
        if !addr.is_multiple_of(size) {
            // The core splits unaligned accesses into naturally aligned ones.
            return split(addr, size).fold(0, |acc, (a, s, shift)| acc | (self.read_aligned(ctx, a, s) << shift));
        }
        self.read_aligned(ctx, addr, size)
    }

    pub fn write(&mut self, ctx: &mut DeviceCtx, addr: u32, size: u32, value: u32) {
        if let Some((word, bit)) = bitband_target(addr) {
            // Read-modify-write of the containing word.
            let old = self.read_aligned(ctx, word, 4);
            let new = if value & 1 != 0 { old | (1 << bit) } else { old & !(1 << bit) };
            self.write_aligned(ctx, word, 4, new);
            return;
        }
        if !addr.is_multiple_of(size) {
            for (a, s, shift) in split(addr, size) {
                let mask = if s == 4 { u32::MAX } else { (1 << (8 * s)) - 1 };
                self.write_aligned(ctx, a, s, (value >> shift) & mask);
            }
            return;
        }
        self.write_aligned(ctx, addr, size, value);
    }

    fn read_aligned(&mut self, ctx: &mut DeviceCtx, addr: u32, size: u32) -> u32 {
        if let Some(m) = self.find(addr) {
            let off = addr - m.base;
            let v = m.dev.read(ctx, off, size);
            let ev = m.dev.next_event();
            self.note_access_event(ev);
            return v;
        }
        if self.warned.insert(addr) {
            log::warn!("read from unmodelled peripheral address {addr:#010x} (reads as 0)");
        }
        0
    }

    fn write_aligned(&mut self, ctx: &mut DeviceCtx, addr: u32, size: u32, value: u32) {
        if let Some(m) = self.find(addr) {
            let off = addr - m.base;
            m.dev.write(ctx, off, size, value);
            let ev = m.dev.next_event();
            self.note_access_event(ev);
            return;
        }
        if self.warned.insert(addr) {
            log::warn!("write {value:#x} to unmodelled peripheral address {addr:#010x} (ignored)");
        }
    }

    /// See [`Peripheral::read_stable_until`]; unmapped addresses read as constant 0.
    pub fn read_stable_until(&mut self, addr: u32, clock: &Clock) -> u64 {
        let addr = bitband_target(addr).map_or(addr, |(word, _)| word);
        match self.find(addr) {
            Some(m) => {
                let off = addr - m.base;
                m.dev.read_stable_until(off, clock)
            }
            None => u64::MAX,
        }
    }

    pub fn update(&mut self, ctx: &mut DeviceCtx) {
        for m in &mut self.maps {
            m.dev.update(ctx);
        }
    }

    /// Sets the function that gives the wait states of peripheral accesses
    /// (e.g. clock domain crossing to a slower APB bus).
    pub fn set_wait_states(&mut self, f: WaitStates) {
        self.wait_states = Some(f);
    }

    /// Wait states of an access to `addr`; bit-band aliases count as an
    /// access to their target word (writes as read plus write).
    pub fn wait_states(&self, addr: u32, write: bool, now: u64) -> u64 {
        let Some(f) = &self.wait_states else { return 0 };
        match bitband_target(addr) {
            Some((word, _)) if write => {
                let rd = f(word, false, now);
                rd + f(word, true, now + rd)
            }
            Some((word, _)) => f(word, false, now),
            None => f(addr, write, now),
        }
    }

    fn note_access_event(&mut self, ev: Option<u64>) {
        if let Some(t) = ev {
            self.access_event = Some(self.access_event.map_or(t, |a| a.min(t)));
        }
    }

    /// Earliest next event of the peripherals accessed since the last call.
    /// An access may schedule an event (e.g. the end of a transfer).
    pub fn take_access_event(&mut self) -> Option<u64> {
        self.access_event.take()
    }

    pub fn next_event(&self) -> Option<u64> {
        self.maps.iter().filter_map(|m| m.dev.next_event()).min()
    }

    pub fn reset(&mut self) {
        for m in &mut self.maps {
            m.dev.reset();
        }
    }
}

pub const PERIPH_BITBAND_ALIAS: u32 = 0x4200_0000;
pub const PERIPH_BITBAND_REGION: u32 = 0x4000_0000;
const BITBAND_ALIAS_SIZE: u32 = 0x0200_0000;

/// Maps a bit-band alias address to (word address in the bit-band region, bit).
/// Every alias word stands for one bit: alias = base + byte_offset * 32 + bit * 4.
pub fn bitband_target(addr: u32) -> Option<(u32, u32)> {
    let off = addr.checked_sub(PERIPH_BITBAND_ALIAS).filter(|&o| o < BITBAND_ALIAS_SIZE)?;
    let byte = PERIPH_BITBAND_REGION + off / 32;
    let bit = (off / 4) % 8 + 8 * (byte & 3);
    Some((byte & !3, bit))
}

/// Splits an unaligned access into naturally aligned pieces:
/// (address, size, bit shift of the piece within the value).
fn split(addr: u32, size: u32) -> impl Iterator<Item = (u32, u32, u32)> {
    let end = addr + size;
    let mut a = addr;
    std::iter::from_fn(move || {
        if a >= end {
            return None;
        }
        let mut s = 4;
        while s > 1 && (!a.is_multiple_of(s) || a + s > end) {
            s /= 2;
        }
        let piece = (a, s, 8 * (a - addr));
        a += s;
        Some(piece)
    })
}

/// Helper for peripherals with plain 32-bit registers: merges a narrow write
/// into the existing register value.
pub fn merge_write(old: u32, offset: u32, size: u32, value: u32) -> u32 {
    let shift = (offset & 3) * 8;
    let mask = match size {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => return value,
    } << shift;
    (old & !mask) | ((value << shift) & mask)
}

/// Extracts a narrow read from a 32-bit register value.
pub fn extract_read(reg: u32, offset: u32, size: u32) -> u32 {
    let shift = (offset & 3) * 8;
    match size {
        1 => (reg >> shift) & 0xFF,
        2 => (reg >> shift) & 0xFFFF,
        _ => reg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitband_alias_of_rcc_pllon() {
        // RCC_CR (0x4002_3800) bit 24, as used by the HAL
        assert_eq!(bitband_target(0x4247_0060), Some((0x4002_3800, 24)));
        assert_eq!(bitband_target(0x4000_0000), None);
    }

    #[test]
    fn unaligned_word_is_split_into_halfwords() {
        let pieces: Vec<_> = split(0x1A, 4).collect();
        assert_eq!(pieces, vec![(0x1A, 2, 0), (0x1C, 2, 16)]);
        let pieces: Vec<_> = split(0x1, 4).collect();
        assert_eq!(pieces, vec![(0x1, 1, 0), (0x2, 2, 8), (0x4, 1, 24)]);
    }
}
