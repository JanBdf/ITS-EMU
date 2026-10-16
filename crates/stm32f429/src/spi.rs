//! SPI controller (master mode, 8/16-bit frames, polled operation).
//!
//! Transfers complete immediately when DR is written: the attached
//! [`SpiDevice`] receives the outgoing frame and returns the incoming one.
//! RXNE/OVR behave like the hardware (overrun when a frame arrives while RXNE
//! is still set; cleared by reading DR, then SR), TXE is always set and BSY
//! never, because there is no transmission time.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::rcc::SharedClocks;
use crate::timer::ApbBus;

/// Something on the other side of the SPI bus (usually a board-level router
/// that looks at chip-select pins).
pub trait SpiDevice {
    /// Exchanges one frame: `mosi` is sent, the return value is received.
    fn transfer(&mut self, mosi: u16) -> u16;
}

pub type SpiSlot = Rc<RefCell<Option<Box<dyn SpiDevice>>>>;

const CR1_SPE: u32 = 1 << 6;
const CR1_MSTR: u32 = 1 << 2;
const CR1_DFF: u32 = 1 << 11;
const SR_RXNE: u32 = 1 << 0;
const SR_TXE: u32 = 1 << 1;
const SR_OVR: u32 = 1 << 6;
const CR2_RXNEIE: u32 = 1 << 6;
const CR2_TXEIE: u32 = 1 << 7;
const CR2_ERRIE: u32 = 1 << 5;

pub struct Spi {
    name: &'static str,
    irq: u32,
    bus: ApbBus,
    enable_bit: u32,
    clocks: SharedClocks,
    device: SpiSlot,
    cr1: u32,
    cr2: u32,
    sr: u32,
    rx: u16,
    /// OVR is cleared by a DR read followed by an SR read.
    ovr_dr_read: bool,
    crcpr: u32,
    i2scfgr: u32,
    i2spr: u32,
}

impl Spi {
    pub fn new(name: &'static str, irq: u32, bus: ApbBus, enable_bit: u32, clocks: SharedClocks, device: SpiSlot) -> Self {
        Self {
            name,
            irq,
            bus,
            enable_bit,
            clocks,
            device,
            cr1: 0,
            cr2: 0,
            sr: SR_TXE,
            rx: 0,
            ovr_dr_read: false,
            crcpr: 7,
            i2scfgr: 0,
            i2spr: 2,
        }
    }

    fn clock_enabled(&self) -> bool {
        let c = self.clocks.get();
        let enr = match self.bus {
            ApbBus::Apb1 => c.apb1enr,
            ApbBus::Apb2 => c.apb2enr,
        };
        enr & (1 << self.enable_bit) != 0
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        let pending = (self.cr2 & CR2_RXNEIE != 0 && self.sr & SR_RXNE != 0)
            || (self.cr2 & CR2_TXEIE != 0 && self.sr & SR_TXE != 0)
            || (self.cr2 & CR2_ERRIE != 0 && self.sr & SR_OVR != 0);
        ctx.set_irq(self.irq, pending);
    }

    fn transmit(&mut self, value: u16) {
        if self.cr1 & (CR1_SPE | CR1_MSTR) != (CR1_SPE | CR1_MSTR) {
            if self.cr1 & CR1_SPE != 0 {
                log::warn!("{}: slave mode is not emulated", self.name);
            }
            return;
        }
        let frame = if self.cr1 & CR1_DFF != 0 { value } else { value & 0xFF };
        let received = match self.device.borrow_mut().as_mut() {
            Some(dev) => dev.transfer(frame),
            None => 0xFF, // MISO pulled up / nothing connected
        };
        if self.sr & SR_RXNE != 0 {
            self.sr |= SR_OVR; // previous frame was not read: new data is lost
            self.ovr_dr_read = false;
        } else {
            self.rx = received;
            self.sr |= SR_RXNE;
        }
    }
}

impl Peripheral for Spi {
    fn name(&self) -> &str {
        self.name
    }

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let reg = match off & !3 {
            0x00 => self.cr1,
            0x04 => self.cr2,
            0x08 => {
                let v = self.sr;
                if self.ovr_dr_read && self.sr & SR_OVR != 0 {
                    self.sr &= !SR_OVR;
                    self.ovr_dr_read = false;
                }
                v
            }
            0x0C => {
                self.sr &= !SR_RXNE;
                if self.sr & SR_OVR != 0 {
                    self.ovr_dr_read = true;
                }
                u32::from(self.rx)
            }
            0x10 => self.crcpr,
            0x1C => self.i2scfgr,
            0x20 => self.i2spr,
            _ => 0,
        };
        self.sync_irq(ctx);
        extract_read(reg, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        match off & !3 {
            0x00 => self.cr1 = merge_write(self.cr1, off, size, value) & 0xFFFF,
            0x04 => self.cr2 = merge_write(self.cr2, off, size, value) & 0xF7,
            0x08 => {} // CRCERR (rc_w0) only, CRC is not emulated
            0x0C => self.transmit(value as u16),
            0x10 => self.crcpr = value & 0xFFFF,
            0x1C => {
                self.i2scfgr = merge_write(self.i2scfgr, off, size, value) & 0x0FBF;
                if self.i2scfgr & (1 << 11) != 0 {
                    log::warn!("{}: I2S mode is not emulated", self.name);
                }
            }
            0x20 => self.i2spr = value & 0x3FF,
            _ => {}
        }
        self.sync_irq(ctx);
    }

    fn reset(&mut self) {
        self.cr1 = 0;
        self.cr2 = 0;
        self.sr = SR_TXE;
        self.rx = 0;
        self.ovr_dr_read = false;
        self.crcpr = 7;
        self.i2scfgr = 0;
        self.i2spr = 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rcc::Clocks;
    use emu_core::Clock;
    use emu_core::nvic::Nvic;

    struct Echo(Vec<u16>);
    impl SpiDevice for Echo {
        fn transfer(&mut self, mosi: u16) -> u16 {
            self.0.push(mosi);
            mosi ^ 0xFF
        }
    }

    #[test]
    fn write_byte_then_overrun_and_flush_like_lcd_driver() {
        let clocks = SharedClocks::default();
        clocks.set(Clocks { apb2enr: 1 << 12, ..Clocks::default() });
        let slot: SpiSlot = Rc::new(RefCell::new(Some(Box::new(Echo(vec![])))));
        let mut spi = Spi::new("SPI1", 35, ApbBus::Apb2, 12, clocks, slot);
        let mut clock = Clock::new(1);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        spi.write(&mut ctx, 0x00, 4, 0x0300 | CR1_MSTR | CR1_SPE | (3 << 3));
        // SPI4W_Write_Byte
        assert_ne!(spi.read(&mut ctx, 0x08, 4) & SR_TXE, 0);
        spi.write(&mut ctx, 0x0C, 4, 0x2C);
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & 0xFF, SR_TXE | SR_RXNE);
        assert_eq!(spi.read(&mut ctx, 0x0C, 4), 0x2C ^ 0xFF);
        // SPI4W_Write_Halfword_Seq ignores RX: second byte overruns
        spi.write(&mut ctx, 0x0C, 4, 0x12);
        spi.write(&mut ctx, 0x0C, 4, 0x34);
        assert_ne!(spi.read(&mut ctx, 0x08, 4) & SR_OVR, 0);
        // flush: read DR, read SR clears OVR and RXNE
        spi.read(&mut ctx, 0x0C, 1);
        let sr = spi.read(&mut ctx, 0x08, 4);
        assert_ne!(sr & SR_OVR, 0, "SR read that clears OVR still shows it");
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & (SR_OVR | SR_RXNE), 0);
    }
}
