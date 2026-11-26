//! Memory-mapped peripheral bus.
//!
//! Peripherals register an address window. Accesses to peripheral space that no
//! model claims read as zero and ignore writes (logged once per address), which
//! keeps firmware running when it touches peripherals we do not model yet.

use std::collections::HashSet;

use crate::clock::Clock;
use crate::nvic::Nvic;

/// Memory as a bus master (DMA) sees it: RAM and flash, without Unicorn.
#[derive(Clone, Default)]
pub struct Ram {
    /// Base address, backing store, writable.
    regions: Vec<(u32, *mut u8, u32, bool)>,
}

impl Ram {
    /// Adds `size` bytes at `ptr` as the memory at `base`.
    ///
    /// # Safety
    /// `ptr` must stay valid (and not be accessed concurrently) while the
    /// `Ram` is used.
    pub unsafe fn add(&mut self, base: u32, ptr: *mut u8, size: u32, writable: bool) {
        self.regions.push((base, ptr, size, writable));
    }

    /// The contents of the region that starts at `base`.
    pub fn region_mut(&mut self, base: u32) -> Option<&mut [u8]> {
        let &(_, ptr, len, _) = self.regions.iter().find(|r| r.0 == base)?;
        // SAFETY: valid while the `Ram` is used (see `add`); borrowed mutably with `self`
        Some(unsafe { std::slice::from_raw_parts_mut(ptr, len as usize) })
    }

    fn locate(&self, addr: u32, size: u32) -> Option<(*mut u8, bool)> {
        self.regions.iter().find_map(|&(base, ptr, len, w)| {
            let off = addr.checked_sub(base)?;
            // SAFETY: in bounds of the region
            (off.checked_add(size)? <= len).then(|| (unsafe { ptr.add(off as usize) }, w))
        })
    }

    /// `len` bytes at `addr`, if they lie in one writable region.
    pub fn writable_bytes(&self, addr: u32, len: u32) -> Option<&[u8]> {
        let (p, writable) = self.locate(addr, len)?;
        // SAFETY: `locate` checked the bounds; valid while the `Ram` is used (see `add`)
        writable.then(|| unsafe { std::slice::from_raw_parts(p, len as usize) })
    }

    /// Reads `size` bytes (1, 2, 4), little endian.
    pub fn read(&self, addr: u32, size: u32) -> Option<u32> {
        if !matches!(size, 1 | 2 | 4) {
            return None;
        }
        let (p, _) = self.locate(addr, size)?;
        let mut b = [0u8; 4];
        // SAFETY: `locate` checked the bounds and `size <= 4`
        unsafe { std::ptr::copy_nonoverlapping(p, b.as_mut_ptr(), size as usize) };
        Some(u32::from_le_bytes(b))
    }

    pub fn write(&mut self, addr: u32, size: u32, value: u32) -> bool {
        if !matches!(size, 1 | 2 | 4) {
            return false;
        }
        match self.locate(addr, size) {
            Some((p, true)) => {
                // SAFETY: `locate` checked the bounds and `size <= 4`
                unsafe { std::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), p, size as usize) };
                true
            }
            _ => false,
        }
    }
}

/// A second bus master besides the core (DMA): moves data between
/// peripherals (through the bus) and memory.
pub trait BusMaster {
    /// Whether there is a transfer to make now.
    fn wants_service(&self) -> bool;
    fn service(&mut self, ctx: &mut DeviceCtx, bus: &mut Bus, ram: &mut Ram);
    /// Optional fast-path flags: when present and all `false`, `wants_service`
    /// is guaranteed to return `false` and the machine skips calling it.
    fn active_flags(&self) -> [Option<std::rc::Rc<std::cell::Cell<bool>>>; 2] {
        [None, None]
    }
}

/// What reset the MCU (reset flags in RCC_CSR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetKind {
    PowerOn,
    /// NRST pin (reset button, debugger).
    Pin,
    /// SYSRESETREQ in SCB_AIRCR.
    Software,
    Watchdog,
}

/// Context handed to peripherals on every access and update.
pub struct DeviceCtx<'a> {
    pub clock: &'a mut Clock,
    pub nvic: &'a mut Nvic,
}

impl DeviceCtx<'_> {
    #[inline]
    pub fn now(&self) -> u64 {
        self.clock.cycles()
    }

    /// Drives the level of external interrupt line `irq`.
    #[inline]
    pub fn set_irq(&mut self, irq: u32, high: bool) {
        self.nvic.set_irq_level(irq, high);
    }

    /// Resets the MCU once the current instruction is done.
    pub fn request_reset(&mut self, kind: ResetKind) {
        self.nvic.reset_request = Some(kind);
    }

    /// Drives source `src` of an interrupt line that several peripherals share.
    #[inline]
    pub fn set_irq_source(&mut self, irq: u32, src: u8, high: bool) {
        self.nvic.set_irq_source(irq, src, high);
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

    /// Whether [`Self::update`] or [`Self::next_event`] needs to be called
    /// during periodic bus sweeps. Peripherals that only react to register
    /// accesses return `false` so the bus skips them on every slice.
    fn has_time_events(&self) -> bool {
        true
    }

    /// Whether this peripheral stays completely idle (no events, `update` is a
    /// no-op) after reset until software writes one of its registers.
    fn idle_until_written(&self) -> bool {
        false
    }

    /// Writes to the register at `offset` change nothing (registers of a
    /// stub that ignores them). Lets idle polling loops that write such a
    /// register be fast-forwarded. Conservative default: false.
    fn write_is_inert(&self, _offset: u32) -> bool {
        false
    }

    fn reset(&mut self) {}

    /// Reset of the MCU; peripherals that keep state across some resets (reset
    /// flags) look at `kind`.
    fn system_reset(&mut self, _kind: ResetKind) {
        self.reset();
    }
}

struct Mapping {
    base: u32,
    size: u32,
    dev: Box<dyn Peripheral>,
    /// Included in `Bus::timed` for `update` / `next_event` sweeps.
    active: bool,
}

/// Extra cycles (bus wait states) of an access to `addr` (`true`: write)
/// that starts at the given cycle.
pub type WaitStates = Box<dyn Fn(u32, bool, u64) -> u64>;

#[derive(Default)]
pub struct Bus {
    maps: Vec<Mapping>,
    /// Indices into `maps` of peripherals with `has_time_events() == true`.
    timed: Vec<usize>,
    /// Index of the mapping found last (polling loops hit the same one).
    last: usize,
    /// Devices without registers (circuitry on the pins, see [`Bus::attach_device`]).
    devices: Vec<Box<dyn Peripheral>>,
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
        let active = dev.has_time_events() && !dev.idle_until_written();
        let pos = self.maps.partition_point(|m| m.base < base);
        self.maps.insert(pos, Mapping { base, size, dev, active });
        self.rebuild_timed();
    }

    fn rebuild_timed(&mut self) {
        self.timed.clear();
        for (i, m) in self.maps.iter().enumerate() {
            if m.active {
                self.timed.push(i);
            }
        }
    }

    /// Adds a device without registers (external circuitry on the pins): it
    /// only takes part in time keeping (`update`, `next_event`) and resets.
    pub fn attach_device(&mut self, dev: Box<dyn Peripheral>) {
        self.devices.push(dev);
    }

    #[inline]
    fn find_idx(&mut self, addr: u32) -> Option<usize> {
        let hit = self.maps.get(self.last).is_some_and(|m| addr.wrapping_sub(m.base) < m.size);
        if !hit {
            let idx = self.maps.partition_point(|m| m.base <= addr).checked_sub(1)?;
            if addr - self.maps[idx].base >= self.maps[idx].size {
                return None;
            }
            self.last = idx;
        }
        Some(self.last)
    }

    #[inline]
    fn find(&mut self, addr: u32) -> Option<&mut Mapping> {
        let idx = self.find_idx(addr)?;
        Some(&mut self.maps[idx])
    }

    #[inline]
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

    #[inline]
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

    #[inline]
    fn read_aligned(&mut self, ctx: &mut DeviceCtx, addr: u32, size: u32) -> u32 {
        if let Some(m) = self.find(addr) {
            let off = addr - m.base;
            let v = m.dev.read(ctx, off, size);
            if m.active {
                let ev = m.dev.next_event();
                self.note_access_event(ev);
            }
            return v;
        }
        if self.warned.insert(addr) {
            log::warn!("read from unmodelled peripheral address {addr:#010x} (reads as 0)");
        }
        0
    }

    #[inline]
    fn write_aligned(&mut self, ctx: &mut DeviceCtx, addr: u32, size: u32, value: u32) {
        if let Some(idx) = self.find_idx(addr) {
            let m = &mut self.maps[idx];
            if !m.active && m.dev.has_time_events() {
                m.active = true;
                self.timed.push(idx);
            }
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

    /// See [`Peripheral::write_is_inert`]; writes to unmapped addresses are ignored.
    pub fn write_is_inert(&mut self, addr: u32) -> bool {
        if bitband_target(addr).is_some() {
            return false;
        }
        match self.find(addr) {
            Some(m) => {
                let off = addr - m.base;
                m.dev.write_is_inert(off)
            }
            None => true,
        }
    }

    pub fn update(&mut self, ctx: &mut DeviceCtx) {
        for &i in &self.timed {
            self.maps[i].dev.update(ctx);
        }
        for d in &mut self.devices {
            d.update(ctx);
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
        let mut best: Option<u64> = None;
        for &i in &self.timed {
            if let Some(t) = self.maps[i].dev.next_event() {
                best = Some(best.map_or(t, |b| b.min(t)));
            }
        }
        for d in &self.devices {
            if let Some(t) = d.next_event() {
                best = Some(best.map_or(t, |b| b.min(t)));
            }
        }
        best
    }

    pub fn reset(&mut self, kind: ResetKind) {
        for m in &mut self.maps {
            m.dev.system_reset(kind);
            m.active = m.dev.has_time_events() && !m.dev.idle_until_written();
        }
        for d in &mut self.devices {
            d.system_reset(kind);
        }
        self.rebuild_timed();
    }
}

pub const PERIPH_BITBAND_ALIAS: u32 = 0x4200_0000;
pub const PERIPH_BITBAND_REGION: u32 = 0x4000_0000;
const BITBAND_ALIAS_SIZE: u32 = 0x0200_0000;

/// Maps a bit-band alias address to (word address in the bit-band region, bit).
/// Every alias word stands for one bit: alias = base + byte_offset * 32 + bit * 4.
#[inline]
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
#[inline]
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
#[inline]
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
