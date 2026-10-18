//! Flash interface (RM0090 §3.5–3.9): access control register and the
//! programming/erase interface of the 2 MB dual-bank flash.
//!
//! The registers ([`FlashInterface`], on the bus) and the memory side
//! ([`FlashProgrammer`], the core's [`FlashController`]) share a
//! [`FlashState`]. Operations take their typical datasheet duration
//! (STM32F429 DS: sector erase 16/64/128 KB at x32 250/550/1000 ms, mass
//! erase of a bank 8 s, word programming 16 µs); BSY stays set that long.
//!
//! Deviations: programming bits that are not erased stores the new value
//! (the cells would end up as old AND new); a store without PG or with a
//! wrong size faults instead of only setting PGSERR/PGPERR; option bytes
//! cannot be changed.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, FlashController, Peripheral};

use crate::rcc::SharedClocks;

const KEY1: u32 = 0x4567_0123;
const KEY2: u32 = 0xCDEF_89AB;
const OPTKEY1: u32 = 0x0819_2A3B;
const OPTKEY2: u32 = 0x4C5D_6E7F;

const SR_EOP: u32 = 1 << 0;
const SR_OPERR: u32 = 1 << 1;
const SR_WRPERR: u32 = 1 << 4;
const SR_PGAERR: u32 = 1 << 5;
const SR_PGPERR: u32 = 1 << 6;
const SR_PGSERR: u32 = 1 << 7;
const SR_RDERR: u32 = 1 << 8;
const SR_BSY: u32 = 1 << 16;
const SR_ERRORS: u32 = SR_OPERR | SR_WRPERR | SR_PGAERR | SR_PGPERR | SR_PGSERR | SR_RDERR;

const CR_PG: u32 = 1 << 0;
const CR_SER: u32 = 1 << 1;
const CR_MER: u32 = 1 << 2;
const CR_MER1: u32 = 1 << 15;
const CR_STRT: u32 = 1 << 16;
const CR_EOPIE: u32 = 1 << 24;
const CR_ERRIE: u32 = 1 << 25;
const CR_LOCK: u32 = 1 << 31;
const CR_WRITABLE: u32 = 0x0300_83FF; // PG SER MER SNB PSIZE MER1 EOPIE ERRIE

const OPTCR_OPTLOCK: u32 = 1 << 0;
const OPTCR_RESET: u32 = 0x0FFF_AAED;

/// FLASH global interrupt.
const IRQ: u32 = 4;
const BANK_SIZE: u32 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FlashState {
    acr: u32,
    cr: u32,
    /// EOP and error flags.
    sr: u32,
    optcr: u32,
    optcr1: u32,
    /// Next expected unlock key: 0 = KEY1, 1 = KEY2; `None` after a wrong
    /// key (locked until reset).
    key_stage: Option<u8>,
    opt_key_stage: Option<u8>,
    /// The running operation ends at this cycle (BSY until then).
    busy_until: u64,
    /// EOP is set when the running operation ends (EOPIE was set).
    eop_on_end: bool,
    /// Bytes to erase (offsets into flash), applied by [`FlashProgrammer`].
    erase: Option<Range<u32>>,
}

impl Default for FlashState {
    fn default() -> Self {
        Self {
            acr: 0,
            cr: CR_LOCK,
            sr: 0,
            optcr: OPTCR_RESET,
            optcr1: 0x0FFF_0000,
            key_stage: Some(0),
            opt_key_stage: Some(0),
            busy_until: 0,
            eop_on_end: false,
            erase: None,
        }
    }
}

impl FlashState {
    fn busy(&self, now: u64) -> bool {
        now < self.busy_until
    }

    /// The running operation has ended: EOP if requested.
    fn settle(&mut self, now: u64) {
        if !self.busy(now) && self.eop_on_end {
            self.eop_on_end = false;
            self.sr |= SR_EOP;
        }
    }

    fn sr(&self, now: u64) -> u32 {
        self.sr | if self.busy(now) { SR_BSY } else { 0 }
    }

    fn cr(&self, now: u64) -> u32 {
        // STRT stays set until BSY clears
        if self.busy(now) { self.cr } else { self.cr & !CR_STRT }
    }

    fn start(&mut self, now: u64, cycles: u64) {
        self.busy_until = self.busy_until.max(now) + cycles;
        self.eop_on_end = self.cr & CR_EOPIE != 0;
    }

    fn irq_level(&self) -> bool {
        (self.cr & CR_EOPIE != 0 && self.sr & SR_EOP != 0) || (self.cr & CR_ERRIE != 0 && self.sr & SR_OPERR != 0)
    }

    /// Bytes per programming access (CR.PSIZE).
    fn psize(&self) -> u32 {
        1 << ((self.cr >> 8) & 3)
    }
}

pub type SharedFlash = Rc<RefCell<FlashState>>;

/// Offset and length of sector `snb` (CR.SNB: 0–11 bank 1, 16–27 bank 2).
fn sector(snb: u32) -> Option<Range<u32>> {
    let (bank, n) = match snb {
        0..=11 => (0, snb),
        16..=27 => (1, snb - 16),
        _ => return None,
    };
    const K: u32 = 1024;
    let (off, len) = match n {
        0..=3 => (n * 16 * K, 16 * K),
        4 => (64 * K, 64 * K),
        _ => ((n - 4) * 128 * K, 128 * K),
    };
    let start = bank * BANK_SIZE + off;
    Some(start..start + len)
}

/// Typical erase time in ms of `len` bytes at parallelism `psize` bytes.
fn erase_ms(len: u32, psize: u32) -> u64 {
    // x8, x16, x32, x64
    let row = match len {
        0..=0x4000 => [400, 300, 250, 200],
        0x4001..=0x10000 => [1200, 700, 550, 450],
        0x10001..=0x20000 => [2000, 1300, 1000, 800],
        _ => [16_000, 11_000, 8000, 6500], // bank (mass erase)
    };
    row[psize.trailing_zeros().min(3) as usize]
}

/// The FLASH registers at 0x4002_3C00.
pub struct FlashInterface {
    state: SharedFlash,
}

impl FlashInterface {
    pub fn new(state: SharedFlash) -> Self {
        Self { state }
    }

    /// Configured number of wait states.
    pub fn latency(&self) -> u32 {
        self.state.borrow().acr & 0xF
    }

    fn write_cr(s: &mut FlashState, ctx: &mut DeviceCtx, v: u32) {
        let now = ctx.now();
        if s.cr & CR_LOCK != 0 {
            return; // ignored while locked
        }
        s.cr = (v & CR_WRITABLE) | (v & CR_LOCK) | (s.cr(now) & CR_STRT);
        if v & CR_LOCK != 0 {
            s.key_stage = s.key_stage.map(|_| 0);
        }
        if v & CR_STRT == 0 || s.busy(now) {
            return;
        }
        let psize = s.psize();
        let ms = |len| ctx.clock.ns_to_cycles(erase_ms(len, psize) * 1_000_000);
        let range = if v & CR_SER != 0 {
            match sector((v >> 3) & 0x1F) {
                Some(r) => r,
                None => {
                    s.sr |= SR_PGSERR;
                    return;
                }
            }
        } else if v & (CR_MER | CR_MER1) != 0 {
            let start = if v & CR_MER != 0 { 0 } else { BANK_SIZE };
            let end = if v & CR_MER1 != 0 { 2 * BANK_SIZE } else { BANK_SIZE };
            start..end
        } else {
            return; // STRT without an erase operation
        };
        let cycles = if range.len() as u32 > BANK_SIZE { 2 * ms(BANK_SIZE + 1) } else { ms(range.len() as u32) };
        log::debug!("flash: erase {:#x}..{:#x}", range.start, range.end);
        s.cr |= CR_STRT;
        s.erase = Some(range);
        s.start(now, cycles);
    }
}

impl Peripheral for FlashInterface {
    fn name(&self) -> &str {
        "FLASH"
    }

    fn read_stable_until(&self, offset: u32, clock: &Clock) -> u64 {
        let s = self.state.borrow();
        match offset & !3 {
            0x0C | 0x10 if s.busy(clock.cycles()) => s.busy_until,
            _ => u64::MAX,
        }
    }

    fn next_event(&self) -> Option<u64> {
        let s = self.state.borrow();
        (s.eop_on_end && s.cr & CR_EOPIE != 0).then_some(s.busy_until)
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        let mut s = self.state.borrow_mut();
        s.settle(ctx.now());
        ctx.set_irq(IRQ, s.irq_level());
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        let now = ctx.now();
        let mut s = self.state.borrow_mut();
        s.settle(now);
        let reg = match off & !3 {
            0x00 => s.acr,
            0x0C => s.sr(now),
            0x10 => s.cr(now),
            0x14 => s.optcr,
            0x18 => s.optcr1,
            _ => 0, // KEYR, OPTKEYR (write-only)
        };
        extract_read(reg, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        let now = ctx.now();
        let mut s = self.state.borrow_mut();
        s.settle(now);
        match off & !3 {
            // ICRST/DCRST are self-clearing
            0x00 => s.acr = merge_write(s.acr, off, size, value) & 0x070F,
            0x04 => {
                s.key_stage = match (s.key_stage, value) {
                    (Some(0), KEY1) => Some(1),
                    (Some(1), KEY2) => {
                        s.cr &= !CR_LOCK;
                        Some(0)
                    }
                    // writing a key while unlocked or a wrong key: locked until reset
                    _ => {
                        log::warn!("flash: wrong unlock sequence (FLASH_KEYR = {value:#010x}): locked until reset");
                        s.cr |= CR_LOCK;
                        None
                    }
                };
            }
            0x08 => {
                s.opt_key_stage = match (s.opt_key_stage, value) {
                    (Some(0), OPTKEY1) => Some(1),
                    (Some(1), OPTKEY2) => {
                        s.optcr &= !OPTCR_OPTLOCK;
                        Some(0)
                    }
                    _ => {
                        s.optcr |= OPTCR_OPTLOCK;
                        None
                    }
                };
            }
            0x0C => {
                // EOP and errors: write 1 to clear
                let v = merge_write(0, off, size, value);
                s.sr &= !(v & (SR_EOP | SR_ERRORS));
            }
            0x10 => {
                let v = merge_write(s.cr(now), off, size, value);
                Self::write_cr(&mut s, ctx, v);
            }
            0x14 if s.optcr & OPTCR_OPTLOCK == 0 => {
                let v = merge_write(s.optcr, off, size, value);
                if v & 2 != 0 {
                    log::warn!("flash: option byte programming (OPTSTRT) is not emulated");
                }
                s.optcr = v & !2;
            }
            _ => {}
        }
        ctx.set_irq(IRQ, s.irq_level());
    }

    fn reset(&mut self) {
        *self.state.borrow_mut() = FlashState::default();
    }
}

/// The memory side of the flash interface: programming accesses and erasing.
pub struct FlashProgrammer {
    state: SharedFlash,
    clocks: SharedClocks,
}

impl FlashProgrammer {
    pub fn new(state: SharedFlash, clocks: SharedClocks) -> Self {
        Self { state, clocks }
    }
}

impl FlashController for FlashProgrammer {
    fn store(&mut self, now: u64, offset: u32, size: u32, value: u32) -> bool {
        let mut s = self.state.borrow_mut();
        s.settle(now);
        let error = if s.cr & CR_LOCK != 0 {
            Some(SR_WRPERR)
        } else if s.cr & CR_PG == 0 {
            Some(SR_PGSERR)
        } else if size != s.psize().min(4) {
            Some(SR_PGPERR)
        } else if !offset.is_multiple_of(size) {
            Some(SR_PGAERR)
        } else {
            None
        };
        if let Some(e) = error {
            log::debug!("flash: store of {value:#x} ({size} bytes) at offset {offset:#x} refused (SR {e:#x})");
            s.sr |= e;
            return false;
        }
        // word programming: 16 µs
        let cycles = u64::from(self.clocks.get().hclk) * 16 / 1_000_000;
        s.start(now, cycles.max(1));
        true
    }

    fn service(&mut self, now: u64, flash: &mut [u8]) -> Option<Range<u32>> {
        let mut s = self.state.borrow_mut();
        s.settle(now);
        let r = s.erase.take()?;
        flash[r.start as usize..r.end as usize].fill(0xFF);
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sector_map_of_2mb_dual_bank_device() {
        assert_eq!(sector(0), Some(0..0x4000));
        assert_eq!(sector(4), Some(0x1_0000..0x2_0000));
        assert_eq!(sector(5), Some(0x2_0000..0x4_0000));
        assert_eq!(sector(11), Some(0xE_0000..0x10_0000));
        assert_eq!(sector(16), Some(0x10_0000..0x10_4000));
        assert_eq!(sector(27), Some(0x1E_0000..0x20_0000));
        assert_eq!(sector(12), None);
    }
}
