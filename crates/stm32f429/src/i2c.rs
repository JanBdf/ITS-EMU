//! I2C controllers in master mode (7-bit addresses), polled or with interrupts.
//!
//! Devices on the bus implement [`I2cDevice`]; an address nobody answers is
//! not acknowledged (AF), like the empty I2C header of the ITS-Board. Bus
//! timing follows the SCL period from CCR, as measured on the board (HwCheck
//! `i2c`): a byte until its acknowledge takes 8 SCL periods, STOP 0.6. Without
//! SCL/SDA routed to the controller (alternate function 4) the bus looks
//! busy, as on the board.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::gpio::{PinMode, SharedGpio, port};
use crate::rcc::SharedClocks;

/// A device on an I2C bus.
pub trait I2cDevice {
    /// START (or repeated START) with `addr` (7 bit); returns the acknowledge.
    fn start(&mut self, addr: u8, read: bool) -> bool;
    /// A byte from the master; returns the acknowledge.
    fn write(&mut self, byte: u8) -> bool;
    /// A byte to the master (who acknowledges it if `ack`).
    fn read(&mut self, ack: bool) -> u8;
    fn stop(&mut self) {}
}

pub type I2cBus = Rc<RefCell<Vec<Box<dyn I2cDevice>>>>;

const CR1_PE: u32 = 1 << 0;
const CR1_START: u32 = 1 << 8;
const CR1_STOP: u32 = 1 << 9;
const CR1_ACK: u32 = 1 << 10;
const CR1_SWRST: u32 = 1 << 15;
const CR2_ITERREN: u32 = 1 << 8;
const CR2_ITEVTEN: u32 = 1 << 9;
const CR2_ITBUFEN: u32 = 1 << 10;
const SR1_SB: u32 = 1 << 0;
const SR1_ADDR: u32 = 1 << 1;
const SR1_BTF: u32 = 1 << 2;
const SR1_RXNE: u32 = 1 << 6;
const SR1_TXE: u32 = 1 << 7;
const SR1_AF: u32 = 1 << 10;
const SR2_MSL: u32 = 1 << 0;
const SR2_BUSY: u32 = 1 << 1;
const SR2_TRA: u32 = 1 << 2;
const CCR_FS: u32 = 1 << 15;
const CCR_DUTY: u32 = 1 << 14;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pending {
    Start,
    /// Address byte or data byte sent; its acknowledge arrives.
    Byte(u8),
    /// A byte received from the device.
    Receive,
    Stop,
}

pub struct I2c {
    name: &'static str,
    ev_irq: u32,
    er_irq: u32,
    enable_bit: u32,
    clocks: SharedClocks,
    gpio: SharedGpio,
    /// SCL and SDA pins that can carry this controller's signals (AF4).
    scl: &'static [(char, u32)],
    sda: &'static [(char, u32)],
    devices: I2cBus,
    cr1: u32,
    cr2: u32,
    oar1: u32,
    oar2: u32,
    dr: u32,
    sr1: u32,
    sr2: u32,
    ccr: u32,
    trise: u32,
    fltr: u32,
    /// SR1 was read since the flag that needs SR1 + DR/SR2 to clear.
    sr1_read: bool,
    /// A device acknowledged the address (it takes part in the transfer).
    addressed: bool,
    /// The byte being sent is an address.
    addr_phase: bool,
    /// Event in progress and the cycle it completes.
    pending: Option<(Pending, u64)>,
}

impl I2c {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &'static str,
        ev_irq: u32,
        enable_bit: u32,
        clocks: SharedClocks,
        gpio: SharedGpio,
        scl: &'static [(char, u32)],
        sda: &'static [(char, u32)],
        devices: I2cBus,
    ) -> Self {
        let mut i = Self {
            name,
            ev_irq,
            er_irq: ev_irq + 1,
            enable_bit,
            clocks,
            gpio,
            scl,
            sda,
            devices,
            cr1: 0,
            cr2: 0,
            oar1: 0,
            oar2: 0,
            dr: 0,
            sr1: 0,
            sr2: 0,
            ccr: 0,
            trise: 0,
            fltr: 0,
            sr1_read: false,
            addressed: false,
            addr_phase: false,
            pending: None,
        };
        i.reset_regs();
        i
    }

    fn reset_regs(&mut self) {
        (self.cr1, self.cr2, self.oar1, self.oar2, self.dr, self.sr1) = (0, 0, 0, 0, 0, 0);
        (self.sr2, self.ccr, self.trise, self.fltr) = (SR2_BUSY, 0, 2, 0);
        (self.sr1_read, self.addressed, self.addr_phase, self.pending) = (false, false, false, None);
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().apb1enr & (1 << self.enable_bit) != 0
    }

    /// SCL and SDA are routed to the controller (alternate function 4).
    fn lines_connected(&self) -> bool {
        let g = self.gpio.borrow();
        let af4 = |&(p, pin): &(char, u32)| {
            let ps = &g.ports[port(p)];
            ps.mode(pin) == PinMode::Alternate && (ps.afr[(pin / 8) as usize] >> (4 * (pin % 8))) & 0xF == 4
        };
        self.scl.iter().any(af4) && self.sda.iter().any(af4)
    }

    /// CPU cycles per PCLK1 cycle.
    fn cpu_per_pclk(&self, ctx: &DeviceCtx) -> u64 {
        (u64::from(ctx.clock.cpu_hz()) / u64::from(self.clocks.get().pclk1.max(1))).max(1)
    }

    /// SCL period in PCLK1 cycles.
    fn scl_period(&self) -> u64 {
        let ccr = u64::from((self.ccr & 0xFFF).max(1));
        match (self.ccr & CCR_FS != 0, self.ccr & CCR_DUTY != 0) {
            (false, _) => 2 * ccr,
            (true, false) => 3 * ccr,
            (true, true) => 25 * ccr,
        }
    }

    /// CPU cycles of a byte transfer up to its acknowledge.
    fn byte_cycles(&self, ctx: &DeviceCtx) -> u64 {
        (8 * self.scl_period() + 8) * self.cpu_per_pclk(ctx)
    }

    fn schedule(&mut self, what: Pending, ctx: &DeviceCtx) {
        let cycles = match what {
            Pending::Start => 10 * self.cpu_per_pclk(ctx),
            Pending::Byte(_) | Pending::Receive => self.byte_cycles(ctx),
            Pending::Stop => (self.scl_period() * 3 / 5).max(1) * self.cpu_per_pclk(ctx),
        };
        self.pending = Some((what, ctx.now() + cycles));
    }

    fn complete(&mut self, what: Pending, ctx: &DeviceCtx) {
        match what {
            Pending::Start => {
                self.cr1 &= !CR1_START;
                self.sr1 |= SR1_SB;
                self.sr2 |= SR2_MSL | SR2_BUSY;
                self.sr1_read = false;
            }
            Pending::Byte(byte) if self.addr_phase => {
                self.addr_phase = false;
                let (addr, read) = (byte >> 1, byte & 1 != 0);
                let mut devices = self.devices.borrow_mut();
                let ack = devices.iter_mut().any(|d| d.start(addr, read));
                drop(devices);
                if ack {
                    self.addressed = true;
                    self.sr1 |= SR1_ADDR;
                    self.sr1_read = false;
                    if read {
                        self.sr2 &= !SR2_TRA;
                    } else {
                        self.sr2 |= SR2_TRA;
                    }
                } else {
                    self.sr1 |= SR1_AF;
                }
            }
            Pending::Byte(byte) => {
                let ack = self.addressed && self.devices.borrow_mut().iter_mut().any(|d| d.write(byte));
                if ack {
                    self.sr1 |= SR1_TXE | SR1_BTF;
                } else {
                    self.sr1 |= SR1_AF;
                }
            }
            Pending::Receive => {
                let ack = self.cr1 & CR1_ACK != 0;
                let byte = self.devices.borrow_mut().iter_mut().map(|d| d.read(ack)).next().unwrap_or(0xFF);
                if self.sr1 & SR1_RXNE != 0 {
                    self.sr1 |= SR1_BTF; // waits with the byte in the shift register
                    self.pending = Some((Pending::Receive, u64::MAX));
                    return;
                }
                self.dr = u32::from(byte);
                self.sr1 |= SR1_RXNE;
                if ack && self.cr1 & CR1_STOP == 0 {
                    self.schedule(Pending::Receive, ctx);
                }
            }
            Pending::Stop => {
                self.cr1 &= !CR1_STOP;
                self.sr2 &= !(SR2_MSL | SR2_BUSY | SR2_TRA);
                self.sr1 &= !(SR1_TXE | SR1_BTF);
                self.addressed = false;
                for d in self.devices.borrow_mut().iter_mut() {
                    d.stop();
                }
            }
        }
    }

    fn advance(&mut self, ctx: &mut DeviceCtx) {
        while let Some((what, at)) = self.pending
            && at <= ctx.now()
        {
            self.pending = None;
            self.complete(what, ctx);
            if self.pending.is_none() && self.cr1 & CR1_STOP != 0 && what != Pending::Stop {
                self.schedule(Pending::Stop, ctx);
            }
        }
        self.sync_irq(ctx);
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        let ev = self.cr2 & CR2_ITEVTEN != 0
            && (self.sr1 & (SR1_SB | SR1_ADDR | SR1_BTF) != 0
                || self.cr2 & CR2_ITBUFEN != 0 && self.sr1 & (SR1_TXE | SR1_RXNE) != 0);
        let er = self.cr2 & CR2_ITERREN != 0 && self.sr1 & 0xDF00 != 0;
        ctx.set_irq(self.ev_irq, ev);
        ctx.set_irq(self.er_irq, er);
    }

    fn reg(&self, off: u32) -> u32 {
        match off {
            0x00 => self.cr1,
            0x04 => self.cr2,
            0x08 => self.oar1,
            0x0C => self.oar2,
            0x10 => self.dr,
            0x14 => self.sr1,
            0x18 => self.sr2,
            0x1C => self.ccr,
            0x20 => self.trise,
            0x24 => self.fltr,
            _ => 0,
        }
    }
}

impl Peripheral for I2c {
    fn name(&self) -> &str {
        self.name
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        self.pending.map_or(u64::MAX, |(_, at)| at)
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        self.advance(ctx);
        let reg = off & !3;
        let v = self.reg(reg);
        match reg {
            0x14 => self.sr1_read = true,
            0x18 if self.sr1_read && self.sr1 & SR1_ADDR != 0 => {
                // ADDR cleared: a transmitter may send, a receiver receives
                self.sr1 &= !SR1_ADDR;
                if self.sr2 & SR2_TRA != 0 {
                    self.sr1 |= SR1_TXE;
                } else {
                    self.schedule(Pending::Receive, ctx);
                }
            }
            0x10 => {
                self.sr1 &= !(SR1_RXNE | SR1_BTF);
                if self.pending == Some((Pending::Receive, u64::MAX)) {
                    self.schedule(Pending::Receive, ctx);
                }
            }
            _ => {}
        }
        self.sync_irq(ctx);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        self.advance(ctx);
        let reg = off & !3;
        let v = merge_write(self.reg(reg), off, size, value);
        match reg {
            0x00 => {
                if v & CR1_SWRST != 0 {
                    self.reset_regs();
                    self.cr1 = CR1_SWRST;
                    return;
                }
                let was_on = self.cr1 & CR1_PE != 0;
                self.cr1 = v & 0xBFFB;
                if self.cr1 & CR1_PE == 0 {
                    self.cr1 &= !(CR1_START | CR1_STOP);
                    self.sr1 = 0;
                    self.sr2 &= SR2_BUSY;
                    self.pending = None;
                    return;
                }
                if !was_on && self.lines_connected() {
                    self.sr2 &= !SR2_BUSY;
                }
                if v & CR1_START != 0 && self.pending.is_none() && self.lines_connected() {
                    if self.sr2 & SR2_MSL != 0 || self.sr2 & SR2_BUSY == 0 {
                        self.schedule(Pending::Start, ctx);
                    }
                } else if v & CR1_STOP != 0 && self.pending.is_none() && self.sr2 & SR2_MSL != 0 {
                    self.schedule(Pending::Stop, ctx);
                }
            }
            0x04 => self.cr2 = v & 0x1F3F,
            0x08 => self.oar1 = v & 0x83FF,
            0x0C => self.oar2 = v & 0xFF,
            0x10 => {
                self.dr = v & 0xFF;
                let addressing = self.sr1 & SR1_SB != 0 && self.sr1_read;
                let sending = self.sr2 & SR2_TRA != 0 && self.sr1 & SR1_ADDR == 0 && self.addressed;
                if (addressing || sending) && self.pending.is_none() {
                    self.addr_phase = addressing;
                    self.sr1 &= !(SR1_SB | SR1_TXE | SR1_BTF);
                    self.schedule(Pending::Byte(self.dr as u8), ctx);
                }
            }
            // rc_w0 flags
            0x14 => self.sr1 &= v | !0xDF00,
            0x1C => self.ccr = v & 0xCFFF,
            0x20 => self.trise = v & 0x3F,
            0x24 => self.fltr = v & 0x1F,
            _ => {}
        }
        self.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.pending.is_some_and(|(_, at)| at <= ctx.now()) {
            self.advance(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.pending.map(|(_, at)| at).filter(|&at| at != u64::MAX)
    }

    fn reset(&mut self) {
        self.reset_regs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::GpioBank;
    use crate::rcc::Clocks;
    use emu_core::nvic::Nvic;

    /// A memory with a register pointer, like an EEPROM at address 0x50.
    struct Memory {
        data: Rc<RefCell<[u8; 4]>>,
        ptr: Option<usize>,
    }

    impl I2cDevice for Memory {
        fn start(&mut self, addr: u8, read: bool) -> bool {
            if !read {
                self.ptr = None; // the first byte written sets the pointer
            }
            addr == 0x50
        }
        fn write(&mut self, byte: u8) -> bool {
            match self.ptr {
                None => self.ptr = Some(byte as usize % 4),
                Some(p) => {
                    self.data.borrow_mut()[p] = byte;
                    self.ptr = Some((p + 1) % 4);
                }
            }
            true
        }
        fn read(&mut self, _ack: bool) -> u8 {
            let p = self.ptr.unwrap_or(0);
            self.ptr = Some((p + 1) % 4);
            self.data.borrow()[p]
        }
        fn stop(&mut self) {}
    }

    fn i2c1(devices: I2cBus) -> I2c {
        let clocks = SharedClocks::default();
        clocks.set(Clocks { pclk1: 45_000_000, apb1enr: 1 << 21, ..Clocks::default() });
        let gpio = SharedGpio::default();
        {
            let mut g: std::cell::RefMut<GpioBank> = gpio.borrow_mut();
            let b = &mut g.ports[port('B')];
            b.moder |= 0xA << 16; // PB8, PB9 alternate function
            b.afr[1] |= 0x44;
        }
        I2c::new("I2C1", 31, 21, clocks, gpio, &[('B', 8)], &[('B', 9)], devices)
    }

    /// Runs until no transfer is in progress.
    fn settle(i: &mut I2c, ctx: &mut DeviceCtx) {
        while let Some(at) = i.next_event() {
            ctx.clock.advance(at.saturating_sub(ctx.now()));
            i.update(ctx);
        }
    }

    #[test]
    fn write_and_read_back_a_device() {
        let data = Rc::new(RefCell::new([0u8; 4]));
        let devices = I2cBus::default();
        devices.borrow_mut().push(Box::new(Memory { data: data.clone(), ptr: None }));
        let mut i = i2c1(devices);
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        i.write(&mut ctx, 0x04, 4, 45);
        i.write(&mut ctx, 0x1C, 4, 225);
        i.write(&mut ctx, 0x00, 4, CR1_PE);
        assert_eq!(i.read(&mut ctx, 0x18, 4), 0, "bus free");
        // write register 1 = 0xAB
        i.write(&mut ctx, 0x00, 4, CR1_PE | CR1_START);
        settle(&mut i, &mut ctx);
        assert_eq!(i.read(&mut ctx, 0x14, 4), SR1_SB);
        i.write(&mut ctx, 0x10, 4, 0xA0);
        let t0 = ctx.now();
        settle(&mut i, &mut ctx);
        assert_eq!(ctx.now() - t0, (8 * 450 + 8) * 4, "8 SCL periods");
        assert_eq!(i.read(&mut ctx, 0x14, 4), SR1_ADDR);
        assert_eq!(i.read(&mut ctx, 0x18, 4), SR2_MSL | SR2_BUSY | SR2_TRA);
        for byte in [1, 0xAB] {
            assert_ne!(i.read(&mut ctx, 0x14, 4) & SR1_TXE, 0);
            i.write(&mut ctx, 0x10, 4, byte);
            settle(&mut i, &mut ctx);
        }
        assert_eq!(i.read(&mut ctx, 0x14, 4), SR1_TXE | SR1_BTF);
        i.write(&mut ctx, 0x00, 4, CR1_PE | CR1_STOP);
        settle(&mut i, &mut ctx);
        assert_eq!(i.read(&mut ctx, 0x18, 4), 0);
        assert_eq!(data.borrow()[1], 0xAB);
        // read it back: register pointer 1, repeated start, read one byte
        i.write(&mut ctx, 0x00, 4, CR1_PE | CR1_START);
        settle(&mut i, &mut ctx);
        i.read(&mut ctx, 0x14, 4);
        i.write(&mut ctx, 0x10, 4, 0xA0);
        settle(&mut i, &mut ctx);
        i.read(&mut ctx, 0x14, 4);
        i.read(&mut ctx, 0x18, 4);
        i.write(&mut ctx, 0x10, 4, 1);
        settle(&mut i, &mut ctx);
        i.write(&mut ctx, 0x00, 4, CR1_PE | CR1_START);
        settle(&mut i, &mut ctx);
        i.read(&mut ctx, 0x14, 4);
        i.write(&mut ctx, 0x10, 4, 0xA1);
        settle(&mut i, &mut ctx);
        assert_eq!(i.read(&mut ctx, 0x14, 4), SR1_ADDR);
        i.write(&mut ctx, 0x00, 4, CR1_PE | CR1_STOP); // single byte: NACK + STOP before ADDR is cleared
        i.read(&mut ctx, 0x18, 4);
        settle(&mut i, &mut ctx);
        assert_ne!(i.read(&mut ctx, 0x14, 4) & SR1_RXNE, 0);
        assert_eq!(i.read(&mut ctx, 0x10, 4), 0xAB);
        assert_eq!(i.read(&mut ctx, 0x18, 4), 0, "stopped");
    }

    #[test]
    fn nobody_answers() {
        let mut i = i2c1(I2cBus::default());
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        i.write(&mut ctx, 0x1C, 4, 225);
        i.write(&mut ctx, 0x00, 4, CR1_PE | CR1_START);
        settle(&mut i, &mut ctx);
        i.read(&mut ctx, 0x14, 4);
        i.write(&mut ctx, 0x10, 4, 0xA0);
        settle(&mut i, &mut ctx);
        assert_eq!(i.read(&mut ctx, 0x14, 4), SR1_AF);
        assert_eq!(i.read(&mut ctx, 0x18, 4), SR2_MSL | SR2_BUSY);
    }
}
