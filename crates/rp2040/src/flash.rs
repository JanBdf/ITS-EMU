//! The QSPI flash interface as far as firmware talks to the flash chip
//! directly: the SSI with a W25Q16JV behind it answering the commands the
//! boot stage 2 and the SDK send (status registers, JEDEC and unique ID).
//! Code and data in flash are read through the XIP mapping, not from here.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::{Clock, DeviceCtx};

use crate::regs::{Block, Op};

/// Unique ID of the emulated flash chip (the SDK makes the USB serial number from it).
pub const FLASH_UNIQUE_ID: [u8; 8] = [0xE6, 0x60, 0x58, 0x38, 0x83, 0x2C, 0x41, 0x2B];

#[derive(Default)]
struct FlashChip {
    /// Bytes of the current command (the command byte first).
    cmd: Vec<u8>,
    /// Chip select forced low through IO_QSPI.
    cs_forced_low: bool,
}

impl FlashChip {
    fn select(&mut self) {
        self.cmd.clear();
    }

    /// Clocks one byte in and returns the byte clocked out.
    fn transfer(&mut self, mosi: u8) -> u8 {
        self.cmd.push(mosi);
        let i = self.cmd.len() - 1;
        if i == 0 {
            return 0xFF;
        }
        match self.cmd[0] {
            0x05 => 0x00,                               // status 1: not busy
            0x35 => 0x02,                               // status 2: QE set
            0x9F => [0xEF, 0x40, 0x15][(i - 1).min(2)], // JEDEC ID
            0x4B if i >= 5 => FLASH_UNIQUE_ID.get(i - 5).copied().unwrap_or(0xFF),
            _ => 0x00,
        }
    }
}

type SharedChip = Rc<RefCell<FlashChip>>;

/// XIP_SSI (DW_apb_ssi). Transfers complete at once: each byte written to
/// DR0 puts the flash's answer into the RX FIFO.
pub struct Ssi {
    chip: SharedChip,
    regs: [u32; 0x100 / 4],
    rx: std::collections::VecDeque<u32>,
}

impl Ssi {
    fn new(chip: SharedChip) -> Self {
        Self { chip, regs: [0; 0x40], rx: Default::default() }
    }
}

impl Block for Ssi {
    fn name(&self) -> &str {
        "XIP_SSI"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        match off {
            0x20 => 0,                    // TXFLR
            0x24 => self.rx.len() as u32, // RXFLR
            // SR: TX FIFO not full and empty, RX not empty, never busy
            0x28 => 0b110 | if self.rx.is_empty() { 0 } else { 0b1000 },
            0x58 => 0x5153_5049, // IDR
            0x5C => 0x3430_312A, // SSI_VERSION_ID
            0x60..=0x9C => self.rx.pop_front().unwrap_or(0),
            _ => self.regs[(off / 4) as usize],
        }
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        match off {
            0x60..=0x9C => {
                let mut chip = self.chip.borrow_mut();
                // Without forced chip select, the SSI selects the flash per
                // burst: a write after all answers were read starts a new command.
                if !chip.cs_forced_low && self.rx.is_empty() {
                    chip.select();
                }
                let miso = chip.transfer(value as u8);
                self.rx.push_back(u32::from(miso));
            }
            0x08 if op.apply(self.regs[2], value) & 1 == 0 => {
                // SSIENR off: FIFOs cleared
                self.rx.clear();
                self.regs[2] = 0;
            }
            _ => {
                let r = &mut self.regs[(off / 4) as usize];
                *r = op.apply(*r, value);
            }
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.regs = [0; 0x40];
        self.rx.clear();
    }
}

/// IO_QSPI: the SDK forces the flash's chip select through the SS pin's
/// output override.
pub struct IoQspi {
    chip: SharedChip,
    regs: [u32; 0x58 / 4],
}

impl IoQspi {
    fn new(chip: SharedChip) -> Self {
        Self { chip, regs: [0; 0x58 / 4] }
    }
}

impl Block for IoQspi {
    fn name(&self) -> &str {
        "IO_QSPI"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        let Some(r) = self.regs.get_mut((off / 4) as usize) else { return };
        *r = op.apply(*r, value);
        if off == 0x0C {
            // GPIO_QSPI_SS_CTRL.OUTOVER: 2 = low (selected)
            let low = (*r >> 8) & 3 == 2;
            let mut chip = self.chip.borrow_mut();
            if low && !chip.cs_forced_low {
                chip.select();
            }
            chip.cs_forced_low = low;
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.regs.fill(0);
        self.chip.borrow_mut().cs_forced_low = false;
    }
}

/// The SSI and IO_QSPI sharing one flash chip.
pub fn flash_interface() -> (Ssi, IoQspi) {
    let chip = SharedChip::default();
    (Ssi::new(chip.clone()), IoQspi::new(chip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::nvic::Nvic;

    #[test]
    fn unique_id_with_forced_chip_select() {
        let (mut ssi, mut io) = flash_interface();
        let mut clock = Clock::new(125_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        io.write(&mut ctx, 0x0C, Op::Write, 2 << 8);
        let mut out = Vec::new();
        for b in [0x4B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] {
            ssi.write(&mut ctx, 0x60, Op::Write, b);
            out.push(ssi.read(&mut ctx, 0x60) as u8);
        }
        io.write(&mut ctx, 0x0C, Op::Write, 3 << 8);
        assert_eq!(&out[5..], &FLASH_UNIQUE_ID);
    }
}
