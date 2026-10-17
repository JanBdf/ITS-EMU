//! CRC calculation unit (CRC-32, polynomial 0x04C11DB7, 32-bit words MSB
//! first) and the random number generator.

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::rcc::SharedClocks;

pub struct Crc {
    dr: u32,
    idr: u32,
    clocks: SharedClocks,
}

impl Crc {
    pub fn new(clocks: SharedClocks) -> Self {
        Self { dr: u32::MAX, idr: 0, clocks }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().ahb1enr & (1 << 12) != 0
    }
}

/// CRC-32 (MPEG-2) step for one word.
pub fn crc32_word(mut crc: u32, word: u32) -> u32 {
    crc ^= word;
    for _ in 0..32 {
        crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
    }
    crc
}

impl Peripheral for Crc {
    fn name(&self) -> &str {
        "CRC"
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let v = match off & !3 {
            0x00 => self.dr,
            0x04 => self.idr,
            _ => 0,
        };
        extract_read(v, off, size)
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        match off & !3 {
            0x00 => self.dr = crc32_word(self.dr, value),
            0x04 => self.idr = merge_write(self.idr, off, size, value) & 0xFF,
            0x08 if value & 1 != 0 => self.dr = u32::MAX,
            _ => {}
        }
    }

    fn reset(&mut self) {
        (self.dr, self.idr) = (u32::MAX, 0);
    }
}

/// Random number generator: a new number 40 RNG clocks after the previous
/// one was read (the RNG clock is the PLL's Q output).
pub struct Rng {
    cr: u32,
    ready_at: Option<u64>,
    state: u64,
    clocks: SharedClocks,
}

impl Rng {
    pub fn new(clocks: SharedClocks) -> Self {
        Self { cr: 0, ready_at: None, state: 0x9E37_79B9_7F4A_7C15, clocks }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().ahb2enr & (1 << 6) != 0
    }

    /// CPU cycles for 40 RNG clocks (about 48 MHz).
    fn generation_cycles(ctx: &DeviceCtx) -> u64 {
        (40 * u64::from(ctx.clock.cpu_hz()) / 48_000_000).max(1)
    }

    fn next(&mut self) -> u32 {
        // splitmix64
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as u32
    }

    fn ready(&self, now: u64) -> bool {
        self.ready_at.is_some_and(|t| t <= now)
    }
}

impl Peripheral for Rng {
    fn name(&self) -> &str {
        "RNG"
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        self.ready_at.unwrap_or(u64::MAX)
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let now = ctx.now();
        let v = match off & !3 {
            0x00 => self.cr,
            0x04 => u32::from(self.ready(now)),
            0x08 => {
                if !self.ready(now) {
                    return 0;
                }
                let v = self.next();
                self.ready_at = (self.cr & 4 != 0).then(|| now + Self::generation_cycles(ctx));
                v
            }
            _ => 0,
        };
        let irq = self.cr & 8 != 0 && self.ready(now);
        ctx.set_irq(80, irq);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        if off & !3 == 0x00 {
            let was_on = self.cr & 4 != 0;
            self.cr = merge_write(self.cr, off, size, value) & 0xC;
            if self.cr & 4 != 0 && !was_on && self.ready_at.is_none() {
                self.ready_at = Some(ctx.now() + Self::generation_cycles(ctx));
            }
        }
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        ctx.set_irq(80, self.cr & 8 != 0 && self.ready(ctx.now()));
    }

    fn next_event(&self) -> Option<u64> {
        self.ready_at.filter(|_| self.cr & 8 != 0)
    }

    fn reset(&mut self) {
        (self.cr, self.ready_at) = (0, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_words_as_on_the_board() {
        // measured: DR after writing 0x12345678, then 0
        assert_eq!(crc32_word(u32::MAX, 0x1234_5678), 0xDF8A_8A2B);
        assert_eq!(crc32_word(0xDF8A_8A2B, 0), 0x9FF5_E078);
    }
}
