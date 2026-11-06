//! I2C1 with DMA1 (reception on stream 0, transmission on stream 6,
//! channel 1) and a memory device at address 0x50, driven register by
//! register through the peripheral bus.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::nvic::Nvic;
use emu_core::{Bus, BusMaster, Clock, DeviceCtx, Ram};
use stm32f429::dma::DmaMaster;
use stm32f429::i2c::I2cDevice;

const I2C1: u32 = 0x4000_5400;
const DMA1: u32 = 0x4002_6000;
const RAM: u32 = 0x2000_0000;

/// Bytes written to it, then bytes read from it with the master's acknowledge.
#[derive(Default)]
struct Log {
    written: Vec<u8>,
    acks: Vec<bool>,
}

struct Memory(Rc<RefCell<Log>>);

impl I2cDevice for Memory {
    fn start(&mut self, addr: u8, _read: bool) -> bool {
        addr == 0x50
    }
    fn write(&mut self, byte: u8) -> bool {
        self.0.borrow_mut().written.push(byte);
        true
    }
    fn read(&mut self, ack: bool) -> u8 {
        let mut log = self.0.borrow_mut();
        log.acks.push(ack);
        0x10 + log.acks.len() as u8
    }
}

struct Board {
    bus: Bus,
    dma: DmaMaster,
    clock: Clock,
    nvic: Nvic,
    ram: Ram,
    _mem: Box<[u8]>,
}

impl Board {
    fn new(log: Rc<RefCell<Log>>) -> Self {
        let (bus, mcu) = stm32f429::build();
        mcu.i2c1.borrow_mut().push(Box::new(Memory(log)));
        let mut mem = vec![0u8; 0x100].into_boxed_slice();
        let mut ram = Ram::default();
        // SAFETY: `mem` lives as long as `ram` (both in the board)
        unsafe { ram.add(RAM, mem.as_mut_ptr(), 0x100, true) };
        let mut b =
            Self { bus, dma: DmaMaster(mcu.dma), clock: Clock::new(16_000_000), nvic: Nvic::new(), ram, _mem: mem };
        b.write(0x4002_3830, 1 << 21 | 1 << 1); // AHB1ENR: DMA1, GPIOB
        b.write(0x4002_3840, 1 << 21); // APB1ENR: I2C1
        b.write(0x4002_0400, 0xA << 16); // PB8, PB9 alternate function 4
        b.write(0x4002_0424, 0x44);
        b
    }

    fn write(&mut self, addr: u32, value: u32) {
        let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
        self.bus.write(&mut ctx, addr, 4, value);
        self.service();
    }

    fn read(&mut self, addr: u32) -> u32 {
        let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
        let v = self.bus.read(&mut ctx, addr, 4);
        self.service();
        v
    }

    fn service(&mut self) {
        while self.dma.wants_service() {
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.dma.service(&mut ctx, &mut self.bus, &mut self.ram);
        }
    }

    /// Runs the peripherals' events until `done` or 10 ms.
    fn until(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        while !done(self) {
            let at = self.bus.next_event().expect("an event");
            assert!(at < 160_000, "timeout");
            self.clock.advance(at.saturating_sub(self.clock.cycles()));
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.bus.update(&mut ctx);
            self.service();
        }
    }

    /// START and the address byte; ADDR is left set.
    fn address(&mut self, byte: u32, cr1: u32) {
        self.write(I2C1, cr1 | 1 << 8);
        self.until(|b| b.read(I2C1 + 0x14) & 1 != 0);
        self.write(I2C1 + 0x10, byte);
        self.until(|b| b.read(I2C1 + 0x14) & 2 != 0);
    }

    /// DMA1 stream `s`: channel 1, bytes, memory increment, `dir`, `n` items at RAM.
    fn stream(&mut self, s: u32, dir: u32, n: u32) {
        let base = DMA1 + 0x10 + 0x18 * s;
        self.write(base + 0x08, I2C1 + 0x10);
        self.write(base + 0x0C, RAM);
        self.write(base + 0x04, n);
        self.write(base, 1 << 25 | 1 << 10 | dir << 6 | 1);
    }
}

#[test]
fn reception_by_dma_nacks_the_last_byte_with_last() {
    let log = Rc::new(RefCell::new(Log::default()));
    let mut b = Board::new(log.clone());
    b.write(I2C1 + 0x04, 16 | 1 << 11 | 1 << 12); // 16 MHz, DMAEN, LAST
    b.write(I2C1 + 0x1C, 80);
    b.write(I2C1, 1);
    b.stream(0, 0, 4);
    b.address(0xA1, 1 | 1 << 10);
    b.read(I2C1 + 0x18); // clears ADDR: reception starts
    b.until(|b| b.read(DMA1) & 1 << 5 != 0); // TCIF0
    b.write(I2C1, 1 | 1 << 9); // STOP
    b.until(|b| b.read(I2C1 + 0x18) & 2 == 0);
    assert_eq!(log.borrow().acks, [true, true, true, false]);
    assert_eq!(b.ram.read(RAM, 4), Some(0x1413_1211));
    assert_eq!(b.read(DMA1 + 0x14), 0, "NDTR");
}

#[test]
fn transmission_by_dma() {
    let log = Rc::new(RefCell::new(Log::default()));
    let mut b = Board::new(log.clone());
    b.ram.write(RAM, 4, 0xDDCC_BBAA);
    b.write(I2C1 + 0x04, 16 | 1 << 11); // DMAEN
    b.write(I2C1 + 0x1C, 80);
    b.write(I2C1, 1);
    b.stream(6, 1, 4);
    b.address(0xA0, 1);
    b.read(I2C1 + 0x18); // clears ADDR: TXE requests the first byte
    b.until(|b| b.read(DMA1 + 0x04) & 1 << 21 != 0); // TCIF6
    b.until(|b| b.read(I2C1 + 0x14) & 1 << 2 != 0); // BTF: the last byte is out
    b.write(I2C1, 1 | 1 << 9);
    b.until(|b| b.read(I2C1 + 0x18) & 2 == 0);
    assert_eq!(log.borrow().written, [0xAA, 0xBB, 0xCC, 0xDD]);
}
