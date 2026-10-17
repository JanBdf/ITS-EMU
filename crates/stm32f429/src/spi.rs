//! SPI controller (master mode, 8/16-bit frames, polled operation).
//!
//! Frames take real time: 8 or 16 bit times at f_PCLK / 2^(BR+1). A frame
//! written to DR moves into the shift register at once if it is idle (TXE
//! stays set), otherwise it waits in the TX buffer (TXE = 0) and follows
//! back-to-back. When the last bit is shifted, the attached [`SpiDevice`]
//! receives the outgoing frame and returns the incoming one (slaves such as
//! the ILI9486 sample D/CX and chip select at the last bit), and RXNE is set.
//! RXNE/OVR behave like the hardware (overrun when a frame arrives while
//! RXNE is still set; cleared by reading DR, then SR). BSY is set while a
//! frame is shifted or waiting.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::dma::{DmaLine, SharedDma};
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
const SR_BSY: u32 = 1 << 7;
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
    /// Frame in the shift register and the cycle its last bit is shifted.
    shifting: Option<(u16, u64)>,
    /// Frame waiting in the TX buffer.
    tx_buf: Option<u16>,
    /// OVR is cleared by a DR read followed by an SR read.
    ovr_dr_read: bool,
    crcpr: u32,
    /// SPI2/SPI3 also have an I2S mode (registers only); the others read 0 there.
    i2s: bool,
    /// DMA controllers and the request lines for reception and transmission,
    /// and the requests last signalled.
    dma: Option<(SharedDma, DmaLine, DmaLine)>,
    dma_req: std::cell::Cell<(bool, bool)>,
    i2scfgr: u32,
    i2spr: u32,
}

impl Spi {
    pub fn new(
        name: &'static str,
        irq: u32,
        bus: ApbBus,
        enable_bit: u32,
        clocks: SharedClocks,
        device: SpiSlot,
    ) -> Self {
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
            shifting: None,
            tx_buf: None,
            ovr_dr_read: false,
            crcpr: 7,
            i2s: false,
            dma: None,
            dma_req: Default::default(),
            i2scfgr: 0,
            i2spr: 0,
        }
    }

    pub fn with_dma(mut self, dma: SharedDma, rx: DmaLine, tx: DmaLine) -> Self {
        self.dma = Some((dma, rx, tx));
        self
    }

    pub fn with_i2s(mut self) -> Self {
        self.i2s = true;
        self.i2spr = 2;
        self
    }

    fn pclk(&self) -> u32 {
        let c = self.clocks.get();
        match self.bus {
            ApbBus::Apb1 => c.pclk1,
            ApbBus::Apb2 => c.pclk2,
        }
    }

    /// Duration of one frame in CPU cycles.
    fn frame_cycles(&self, ctx: &DeviceCtx) -> u64 {
        let bits: u64 = if self.cr1 & CR1_DFF != 0 { 16 } else { 8 };
        let div = 2u64 << ((self.cr1 >> 3) & 7);
        let pclk = u64::from(self.pclk().max(1));
        (bits * div * u64::from(ctx.clock.cpu_hz()) / pclk).max(1)
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
        if let Some((dma, rx, tx)) = &self.dma {
            let on = self.cr1 & CR1_SPE != 0;
            let req =
                (on && self.cr2 & 1 != 0 && self.sr & SR_RXNE != 0, on && self.cr2 & 2 != 0 && self.sr & SR_TXE != 0);
            if req != self.dma_req.replace(req) {
                rx.set(dma, req.0);
                tx.set(dma, req.1);
            }
        }
    }

    fn transmit(&mut self, ctx: &DeviceCtx, value: u16) {
        if self.cr1 & (CR1_SPE | CR1_MSTR) != (CR1_SPE | CR1_MSTR) {
            if self.cr1 & CR1_SPE != 0 {
                log::warn!("{}: slave mode is not emulated", self.name);
            }
            return;
        }
        let frame = if self.cr1 & CR1_DFF != 0 { value } else { value & 0xFF };
        if self.shifting.is_none() {
            self.shifting = Some((frame, ctx.now() + self.frame_cycles(ctx)));
        } else {
            // DR written while TXE = 0 overwrites the waiting frame.
            self.tx_buf = Some(frame);
            self.sr &= !SR_TXE;
        }
    }

    /// Completes all frames whose last bit has been shifted by `ctx.now()`.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        while let Some((frame, done)) = self.shifting {
            if done > now {
                break;
            }
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
            self.shifting = self.tx_buf.take().map(|f| (f, done + self.frame_cycles(ctx)));
            self.sr |= SR_TXE;
        }
        self.sync_irq(ctx);
    }

    fn sr(&self) -> u32 {
        if self.shifting.is_some() { self.sr | SR_BSY } else { self.sr }
    }
}

impl Peripheral for Spi {
    fn name(&self) -> &str {
        self.name
    }

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        // State only changes at the events reported by `next_event`.
        u64::MAX
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        self.advance(ctx);
        let reg = match off & !3 {
            0x00 => self.cr1,
            0x04 => self.cr2,
            0x08 => {
                let v = self.sr();
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
            0x1C if self.i2s => self.i2scfgr,
            0x20 if self.i2s => self.i2spr,
            _ => 0,
        };
        self.sync_irq(ctx);
        extract_read(reg, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        self.advance(ctx);
        match off & !3 {
            0x00 => self.cr1 = merge_write(self.cr1, off, size, value) & 0xFFFF,
            0x04 => self.cr2 = merge_write(self.cr2, off, size, value) & 0xF7,
            0x08 => {} // CRCERR (rc_w0) only, CRC is not emulated
            0x0C => self.transmit(ctx, value as u16),
            0x10 => self.crcpr = value & 0xFFFF,
            0x1C if self.i2s => {
                self.i2scfgr = merge_write(self.i2scfgr, off, size, value) & 0x0FBF;
                if self.i2scfgr & (1 << 11) != 0 {
                    log::warn!("{}: I2S mode is not emulated", self.name);
                }
            }
            0x20 if self.i2s => self.i2spr = value & 0x3FF,
            _ => {}
        }
        self.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.advance(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        self.shifting.map(|(_, t)| t)
    }

    fn reset(&mut self) {
        self.shifting = None;
        self.tx_buf = None;
        self.cr1 = 0;
        self.cr2 = 0;
        self.sr = SR_TXE;
        self.rx = 0;
        self.ovr_dr_read = false;
        self.crcpr = 7;
        self.i2scfgr = 0;
        self.i2spr = if self.i2s { 2 } else { 0 };
        if let Some((d, rx, tx)) = &self.dma {
            rx.set(d, false);
            tx.set(d, false);
        }
        self.dma_req.set((false, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rcc::Clocks;
    use emu_core::Clock;
    use emu_core::nvic::Nvic;

    struct Echo(Rc<RefCell<Vec<u16>>>);
    impl SpiDevice for Echo {
        fn transfer(&mut self, mosi: u16) -> u16 {
            self.0.borrow_mut().push(mosi);
            mosi ^ 0xFF
        }
    }

    fn setup() -> (Spi, Rc<RefCell<Vec<u16>>>, Clock, Nvic) {
        let clocks = SharedClocks::default();
        clocks.set(Clocks { pclk2: 90_000_000, apb2enr: 1 << 12, ..Clocks::default() });
        let log = Rc::new(RefCell::new(vec![]));
        let slot: SpiSlot = Rc::new(RefCell::new(Some(Box::new(Echo(log.clone())))));
        let spi = Spi::new("SPI1", 35, ApbBus::Apb2, 12, clocks, slot);
        (spi, log, Clock::new(180_000_000), Nvic::new())
    }

    /// LCD speed of the ITS-Board: BR = 3 (f_PCLK2 / 16), 8-bit frames.
    const LCD_CR1: u32 = 0x0300 | CR1_MSTR | CR1_SPE | (3 << 3);
    /// 8 bits at 90 MHz / 16 in 180 MHz CPU cycles.
    const BYTE: u64 = 256;

    #[test]
    fn write_byte_then_overrun_and_flush_like_lcd_driver() {
        let (mut spi, log, mut clock, mut nvic) = setup();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        spi.write(&mut ctx, 0x00, 4, LCD_CR1);
        // SPI4W_Write_Byte
        assert_ne!(spi.read(&mut ctx, 0x08, 4) & SR_TXE, 0);
        spi.write(&mut ctx, 0x0C, 4, 0x2C);
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & 0xFF, SR_TXE | SR_BSY, "shifting, TX buffer free");
        ctx.clock.advance(BYTE - 1);
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & SR_RXNE, 0);
        assert!(log.borrow().is_empty(), "slave sees the frame at its last bit");
        ctx.clock.advance(1);
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & 0xFF, SR_TXE | SR_RXNE);
        assert_eq!(spi.read(&mut ctx, 0x0C, 4), 0x2C ^ 0xFF);
        // SPI4W_Write_Halfword_Seq ignores RX: second byte overruns
        spi.write(&mut ctx, 0x0C, 4, 0x12);
        spi.write(&mut ctx, 0x0C, 4, 0x34);
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & SR_TXE, 0, "second byte waits in the TX buffer");
        ctx.clock.advance(BYTE);
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & (SR_TXE | SR_BSY), SR_TXE | SR_BSY, "back-to-back");
        ctx.clock.advance(BYTE);
        assert_ne!(spi.read(&mut ctx, 0x08, 4) & SR_OVR, 0);
        assert_eq!(*log.borrow(), [0x2C, 0x12, 0x34]);
        // flush: read DR, read SR clears OVR and RXNE
        spi.read(&mut ctx, 0x0C, 1);
        let sr = spi.read(&mut ctx, 0x08, 4);
        assert_ne!(sr & SR_OVR, 0, "SR read that clears OVR still shows it");
        assert_eq!(spi.read(&mut ctx, 0x08, 4) & (SR_OVR | SR_RXNE | SR_BSY), 0);
    }

    #[test]
    fn frame_time_follows_prescaler_and_frame_size() {
        let (mut spi, _, mut clock, mut nvic) = setup();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        // Touch speed: BR = 6 (f_PCLK2 / 128), 16-bit frames.
        spi.write(&mut ctx, 0x00, 4, CR1_MSTR | CR1_SPE | CR1_DFF | (6 << 3));
        spi.write(&mut ctx, 0x0C, 4, 0xD000);
        assert_eq!(spi.next_event(), Some(16 * 128 * 2));
    }
}
