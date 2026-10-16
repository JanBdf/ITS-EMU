//! USART in asynchronous mode with real frame timing.
//!
//! Bytes take one frame time (start + data + parity + stop bits at the
//! configured baud rate) to shift out and in. Transmitted bytes appear in the
//! [`SerialPort`] when their stop bit is sent; bytes queued by the host are
//! received one frame time apart. Flag semantics follow RM0090: TXE/TC,
//! RXNE, IDLE, ORE, and their clear sequences (SR read followed by DR access).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::rcc::SharedClocks;
use crate::timer::ApbBus;

/// The host side of a serial line (e.g. the ST-LINK virtual COM port).
#[derive(Debug, Default)]
pub struct SerialPort {
    /// Bytes sent by the MCU, not yet consumed by the host.
    pub tx: Vec<u8>,
    /// Bytes the host wants to send to the MCU.
    pub rx: VecDeque<u8>,
}

impl SerialPort {
    pub fn send(&mut self, data: &[u8]) {
        self.rx.extend(data);
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }
}

pub type SharedSerial = Rc<RefCell<SerialPort>>;

const SR_PE: u32 = 1 << 0;
const SR_FE: u32 = 1 << 1;
const SR_NF: u32 = 1 << 2;
const SR_ORE: u32 = 1 << 3;
const SR_IDLE: u32 = 1 << 4;
const SR_RXNE: u32 = 1 << 5;
const SR_TC: u32 = 1 << 6;
const SR_TXE: u32 = 1 << 7;
const SR_ERRORS: u32 = SR_PE | SR_FE | SR_NF | SR_ORE | SR_IDLE;

const CR1_RE: u32 = 1 << 2;
const CR1_TE: u32 = 1 << 3;
const CR1_IDLEIE: u32 = 1 << 4;
const CR1_RXNEIE: u32 = 1 << 5;
const CR1_TCIE: u32 = 1 << 6;
const CR1_TXEIE: u32 = 1 << 7;
const CR1_PCE: u32 = 1 << 10;
const CR1_M: u32 = 1 << 12;
const CR1_UE: u32 = 1 << 13;
const CR1_OVER8: u32 = 1 << 15;
const CR3_EIE: u32 = 1 << 0;

pub struct Usart {
    name: &'static str,
    irq: u32,
    bus: ApbBus,
    enable_bit: u32,
    clocks: SharedClocks,
    port: SharedSerial,
    sr: u32,
    rdr: u16,
    brr: u32,
    cr1: u32,
    cr2: u32,
    cr3: u32,
    gtpr: u32,
    /// Byte waiting in the transmit data register.
    tdr: Option<u16>,
    /// Byte in the transmit shift register and the cycle its stop bit ends.
    shifting: Option<(u16, u64)>,
    /// Cycle at which the next host byte has been received completely.
    rx_done_at: Option<u64>,
    /// Cycle at which the line is detected idle after a reception.
    idle_at: Option<u64>,
    /// Status flags that are cleared by "SR read, then DR read/write".
    sr_read_mask: u32,
    warned: bool,
}

impl Usart {
    pub fn new(name: &'static str, irq: u32, bus: ApbBus, enable_bit: u32, clocks: SharedClocks, port: SharedSerial) -> Self {
        Self {
            name,
            irq,
            bus,
            enable_bit,
            clocks,
            port,
            sr: SR_TXE | SR_TC,
            rdr: 0,
            brr: 0,
            cr1: 0,
            cr2: 0,
            cr3: 0,
            gtpr: 0,
            tdr: None,
            shifting: None,
            rx_done_at: None,
            idle_at: None,
            sr_read_mask: 0,
            warned: false,
        }
    }

    fn clocks_enabled(&self) -> bool {
        let c = self.clocks.get();
        let enr = match self.bus {
            ApbBus::Apb1 => c.apb1enr,
            ApbBus::Apb2 => c.apb2enr,
        };
        enr & (1 << self.enable_bit) != 0
    }

    fn pclk(&self) -> u32 {
        let c = self.clocks.get();
        match self.bus {
            ApbBus::Apb1 => c.pclk1,
            ApbBus::Apb2 => c.pclk2,
        }
    }

    fn baud(&self) -> f64 {
        let mantissa = f64::from(self.brr >> 4);
        let fraction = f64::from(self.brr & 0xF);
        let pclk = f64::from(self.pclk());
        let div = if self.cr1 & CR1_OVER8 != 0 { mantissa + (fraction.min(7.0)) / 8.0 } else { mantissa + fraction / 16.0 };
        let over = if self.cr1 & CR1_OVER8 != 0 { 8.0 } else { 16.0 };
        if div <= 0.0 { 0.0 } else { pclk / (over * div) }
    }

    /// Duration of one frame in CPU cycles.
    fn frame_cycles(&self, ctx: &DeviceCtx) -> u64 {
        let data_bits = if self.cr1 & CR1_M != 0 { 9.0 } else { 8.0 };
        let stop_bits = match (self.cr2 >> 12) & 3 {
            1 => 0.5,
            2 => 2.0,
            3 => 1.5,
            _ => 1.0,
        };
        let bits = 1.0 + data_bits + stop_bits;
        let baud = self.baud();
        if baud <= 0.0 {
            return u64::MAX / 4;
        }
        ((bits / baud) * f64::from(ctx.clock.cpu_hz())).ceil().max(1.0) as u64
    }

    fn enabled(&self, bit: u32) -> bool {
        self.cr1 & (CR1_UE | bit) == (CR1_UE | bit)
    }

    /// Processes everything that happened up to `ctx.now()`.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        // Transmitter
        while let Some((byte, done)) = self.shifting {
            if done > now {
                break;
            }
            self.port.borrow_mut().tx.push(byte as u8);
            self.shifting = self.tdr.take().map(|b| (b, done + self.frame_cycles(ctx)));
            if self.shifting.is_some() {
                self.sr |= SR_TXE;
            } else {
                self.sr |= SR_TC;
            }
        }
        // Receiver
        if !self.enabled(CR1_RE) {
            self.rx_done_at = None;
        } else {
            loop {
                match self.rx_done_at {
                    None => {
                        if self.port.borrow().rx.is_empty() {
                            break;
                        }
                        self.rx_done_at = Some(now + self.frame_cycles(ctx));
                        self.idle_at = None;
                    }
                    Some(t) if t <= now => {
                        let byte = self.port.borrow_mut().rx.pop_front();
                        if let Some(b) = byte {
                            if self.sr & SR_RXNE != 0 {
                                self.sr |= SR_ORE; // previous byte not read: this one is lost
                            } else {
                                self.rdr = u16::from(b);
                                self.sr |= SR_RXNE;
                            }
                        }
                        let frame = self.frame_cycles(ctx);
                        self.idle_at = Some(t + frame);
                        self.rx_done_at = (!self.port.borrow().rx.is_empty()).then_some(t + frame);
                    }
                    Some(_) => break,
                }
            }
            if let Some(t) = self.idle_at
                && t <= now
                && self.rx_done_at.is_none()
            {
                self.sr |= SR_IDLE;
                self.idle_at = None;
            }
        }
        self.sync_irq(ctx);
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        let cr1 = self.cr1;
        let pending = (cr1 & CR1_TXEIE != 0 && self.sr & SR_TXE != 0)
            || (cr1 & CR1_TCIE != 0 && self.sr & SR_TC != 0)
            || (cr1 & CR1_RXNEIE != 0 && self.sr & (SR_RXNE | SR_ORE) != 0)
            || (cr1 & CR1_IDLEIE != 0 && self.sr & SR_IDLE != 0)
            || (self.cr3 & CR3_EIE != 0 && self.sr & (SR_FE | SR_NF | SR_ORE) != 0);
        ctx.set_irq(self.irq, pending && cr1 & CR1_UE != 0);
    }

    fn write_dr(&mut self, ctx: &mut DeviceCtx, value: u32) {
        // "SR read, DR write" clears TC.
        if self.sr_read_mask & SR_TC != 0 {
            self.sr &= !SR_TC;
        }
        self.sr_read_mask = 0;
        if !self.enabled(CR1_TE) {
            return;
        }
        let mask = if self.cr1 & CR1_M != 0 && self.cr1 & CR1_PCE == 0 { 0x1FF } else { 0xFF };
        let byte = (value & mask) as u16;
        self.sr &= !SR_TC;
        if self.shifting.is_none() {
            // Moves straight into the shift register: TXE stays set.
            self.shifting = Some((byte, ctx.now() + self.frame_cycles(ctx)));
        } else {
            if self.tdr.is_some() && !self.warned {
                self.warned = true;
                log::warn!("{}: DR written while TXE = 0, previous byte lost", self.name);
            }
            self.tdr = Some(byte);
            self.sr &= !SR_TXE;
        }
    }

    fn read_dr(&mut self) -> u32 {
        // "SR read, DR read" clears IDLE, ORE, NF, FE, PE.
        self.sr &= !(self.sr_read_mask & SR_ERRORS);
        self.sr_read_mask = 0;
        self.sr &= !SR_RXNE;
        u32::from(self.rdr)
    }

    fn reg(&self, off: u32) -> u32 {
        match off {
            0x00 => self.sr,
            0x04 => u32::from(self.rdr),
            0x08 => self.brr,
            0x0C => self.cr1,
            0x10 => self.cr2,
            0x14 => self.cr3,
            0x18 => self.gtpr,
            _ => 0,
        }
    }
}

impl Peripheral for Usart {
    fn name(&self) -> &str {
        self.name
    }

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        // State only changes at the events reported by `next_event`.
        u64::MAX
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clocks_enabled() {
            return 0;
        }
        self.advance(ctx);
        let v = match off & !3 {
            0x00 => {
                self.sr_read_mask = self.sr;
                self.sr
            }
            0x04 => self.read_dr(),
            other => self.reg(other),
        };
        self.sync_irq(ctx);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clocks_enabled() {
            return;
        }
        self.advance(ctx);
        let aligned = off & !3;
        let v = merge_write(self.reg(aligned), off, size, value);
        match aligned {
            // Only RXNE and TC can be cleared by writing 0; other bits are read-only.
            0x00 => self.sr &= v | !(SR_RXNE | SR_TC),
            0x04 => self.write_dr(ctx, v),
            0x08 => self.brr = v & 0xFFFF,
            0x0C => {
                self.cr1 = v & 0xBFFF;
                if self.cr1 & CR1_UE == 0 {
                    self.shifting = None;
                    self.tdr = None;
                    self.sr |= SR_TXE | SR_TC;
                }
            }
            0x10 => self.cr2 = v & 0x7F7F,
            0x14 => self.cr3 = v & 0x0FFF,
            0x18 => self.gtpr = v & 0xFFFF,
            _ => {}
        }
        self.advance(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.advance(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        let waiting_rx = self.enabled(CR1_RE) && self.rx_done_at.is_none() && !self.port.borrow().rx.is_empty();
        let rx = if waiting_rx { Some(0) } else { self.rx_done_at };
        [self.shifting.map(|(_, t)| t), rx, self.idle_at].into_iter().flatten().min()
    }

    fn reset(&mut self) {
        let port = self.port.clone();
        *self = Self::new(self.name, self.irq, self.bus, self.enable_bit, self.clocks.clone(), port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rcc::Clocks;
    use emu_core::Clock;
    use emu_core::nvic::Nvic;

    fn setup() -> (Usart, SharedSerial, Clock, Nvic) {
        let clocks = SharedClocks::default();
        clocks.set(Clocks { pclk1: 45_000_000, apb1enr: 1 << 18, ..Clocks::default() });
        let port = SharedSerial::default();
        let mut u = Usart::new("USART3", 39, ApbBus::Apb1, 18, clocks, port.clone());
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            u.write(&mut ctx, 0x08, 4, 0x187); // 115200 baud @ 45 MHz (as the driver computes)
            u.write(&mut ctx, 0x0C, 4, CR1_UE | CR1_TE | CR1_RE);
        }
        (u, port, clock, nvic)
    }

    #[test]
    fn transmit_takes_one_frame_per_byte() {
        let (mut u, port, mut clock, mut nvic) = setup();
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            u.write(&mut ctx, 0x04, 4, u32::from(b'H'));
            u.write(&mut ctx, 0x04, 4, u32::from(b'i'));
            assert_eq!(u.read(&mut ctx, 0x00, 4) & (SR_TXE | SR_TC), 0);
        }
        // 10 bits at ~115 kbaud ≈ 86.8 µs ≈ 15 600 cycles
        let frame = u.next_event().unwrap();
        assert!((15_000..16_200).contains(&frame), "frame = {frame}");
        clock.advance(frame);
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        u.update(&mut ctx);
        assert_eq!(port.borrow().tx, b"H");
        assert_ne!(u.read(&mut ctx, 0x00, 4) & SR_TXE, 0);
        ctx.clock.advance(frame);
        u.update(&mut ctx);
        assert_eq!(port.borrow_mut().take_output(), b"Hi");
        assert_ne!(u.read(&mut ctx, 0x00, 4) & SR_TC, 0);
    }

    #[test]
    fn receive_with_overrun_and_idle() {
        let (mut u, port, mut clock, mut nvic) = setup();
        port.borrow_mut().send(b"ab");
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        u.update(&mut ctx);
        let t = u.next_event().unwrap();
        ctx.clock.advance(t);
        u.update(&mut ctx);
        assert_ne!(u.read(&mut ctx, 0x00, 4) & SR_RXNE, 0);
        // Do not read 'a': 'b' overruns
        let t2 = u.next_event().unwrap();
        ctx.clock.advance(t2 - ctx.now());
        u.update(&mut ctx);
        let sr = u.read(&mut ctx, 0x00, 4);
        assert_ne!(sr & SR_ORE, 0);
        assert_eq!(u.read(&mut ctx, 0x04, 4), u32::from(b'a'));
        assert_eq!(u.read(&mut ctx, 0x00, 4) & SR_ORE, 0, "SR read + DR read clears ORE");
        // Line becomes idle one frame later
        let t3 = u.next_event().unwrap();
        ctx.clock.advance(t3 - ctx.now());
        u.update(&mut ctx);
        assert_ne!(u.read(&mut ctx, 0x00, 4) & SR_IDLE, 0);
    }
}
