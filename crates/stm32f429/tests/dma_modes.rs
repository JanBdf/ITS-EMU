//! DMA FIFO mode with different peripheral and memory sizes, and double
//! buffer mode, paced by TIM2's update requests (DMA1 stream 1, channel 3)
//! or memory to memory (DMA2 stream 0).

use emu_core::nvic::Nvic;
use emu_core::{Bus, BusMaster, Clock, DeviceCtx, Ram};
use stm32f429::dma::DmaMaster;

const TIM2: u32 = 0x4000_0000;
const DMA1: u32 = 0x4002_6000;
const DMA2: u32 = 0x4002_6400;
const RAM: u32 = 0x2000_0000;
const CR_EN: u32 = 1;
const CR_PINC: u32 = 1 << 9;
const CR_MINC: u32 = 1 << 10;
const CR_DBM: u32 = 1 << 18;
const CR_CT: u32 = 1 << 19;
const FCR_DMDIS: u32 = 1 << 2;

struct Board {
    bus: Bus,
    dma: DmaMaster,
    clock: Clock,
    nvic: Nvic,
    ram: Ram,
    _mem: Box<[u8]>,
}

impl Board {
    fn new() -> Self {
        let (bus, mcu) = stm32f429::build();
        let mut mem = vec![0u8; 0x100].into_boxed_slice();
        let mut ram = Ram::default();
        // SAFETY: `mem` lives as long as `ram` (both in the board)
        unsafe { ram.add(RAM, mem.as_mut_ptr(), 0x100, true) };
        let mut b =
            Self { bus, dma: DmaMaster(mcu.dma), clock: Clock::new(16_000_000), nvic: Nvic::new(), ram, _mem: mem };
        b.write(0x4002_3830, 3 << 21); // AHB1ENR: DMA1, DMA2
        b.write(0x4002_3840, 1); // APB1ENR: TIM2
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

    /// TIM2 requests a transfer every 100 cycles.
    fn start_tim2(&mut self) {
        self.write(TIM2 + 0x2C, 99);
        self.write(TIM2 + 0x0C, 1 << 8); // UDE
        self.write(TIM2, 1);
    }

    /// Runs `n` TIM2 update requests.
    fn updates(&mut self, n: u64) {
        let until = self.clock.cycles() + 100 * n;
        while self.clock.cycles() < until {
            let at = self.bus.next_event().unwrap_or(until).min(until);
            self.clock.advance(at.saturating_sub(self.clock.cycles()).max(1));
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.bus.update(&mut ctx);
            self.service();
        }
    }

    /// DMA1 stream 1, channel 3 (TIM2_UP).
    fn stream1(&mut self, par: u32, m0: u32, m1: u32, n: u32, fcr: u32, cr: u32) {
        let base = DMA1 + 0x10 + 0x18;
        self.write(base + 0x08, par);
        self.write(base + 0x0C, m0);
        self.write(base + 0x10, m1);
        self.write(base + 0x04, n);
        self.write(base + 0x14, fcr);
        self.write(base, 3 << 25 | cr | CR_EN);
    }
}

#[test]
fn memory_to_memory_packs_bytes_into_words() {
    let mut b = Board::new();
    for i in 0..8 {
        b.ram.write(RAM + i, 1, 0x11 * (i + 1));
    }
    let base = DMA2 + 0x10;
    b.write(base + 0x08, RAM);
    b.write(base + 0x0C, RAM + 0x40);
    b.write(base + 0x04, 8); // items of PSIZE: 8 bytes
    b.write(base + 0x14, FCR_DMDIS | 3);
    // memory to memory, PSIZE = byte, MSIZE = word
    b.write(base, 2 << 6 | CR_PINC | CR_MINC | 2 << 13 | CR_EN);
    assert_eq!(b.ram.read(RAM + 0x40, 4), Some(0x4433_2211));
    assert_eq!(b.ram.read(RAM + 0x44, 4), Some(0x8877_6655));
    assert_eq!(b.ram.read(RAM + 0x48, 4), Some(0), "two words only");
    assert_eq!(b.read(DMA2) & 1 << 5, 1 << 5, "TCIF0");
    assert_eq!(b.read(base + 0x14) >> 3 & 7, 4, "FIFO empty");
}

#[test]
fn memory_to_peripheral_unpacks_words_into_half_words() {
    let mut b = Board::new();
    b.ram.write(RAM, 4, 0xBBBB_AAAA);
    b.ram.write(RAM + 4, 4, 0xDDDD_CCCC);
    // to half words at RAM + 0x40 (standing in for a peripheral), PSIZE = half word, MSIZE = word
    b.stream1(RAM + 0x40, RAM, 0, 4, FCR_DMDIS, 1 << 6 | CR_PINC | CR_MINC | 1 << 11 | 2 << 13);
    b.start_tim2();
    b.updates(1);
    assert_eq!(b.ram.read(RAM + 0x40, 4), Some(0x0000_AAAA), "one half word per request");
    assert_eq!(b.read(DMA1 + 0x10 + 0x18 + 0x14) >> 3 & 7, 0, "half a word in the FIFO");
    b.updates(3);
    assert_eq!(b.ram.read(RAM + 0x40, 4), Some(0xBBBB_AAAA));
    assert_eq!(b.ram.read(RAM + 0x44, 4), Some(0xDDDD_CCCC));
    assert_eq!(b.read(DMA1 + 0x10 + 0x18 + 0x04), 0, "NDTR");
}

#[test]
fn double_buffer_mode_alternates_the_buffers() {
    let mut b = Board::new();
    b.ram.write(RAM, 4, 0x1234_5678);
    // words from RAM to two buffers of two words
    b.stream1(RAM, RAM + 0x40, RAM + 0x80, 2, 0, 2 << 11 | 2 << 13 | CR_MINC | CR_DBM);
    b.start_tim2();
    b.updates(2);
    let cr = DMA1 + 0x10 + 0x18;
    assert_eq!(b.read(cr) & CR_CT, CR_CT, "on to M1AR");
    assert_eq!(b.read(DMA1) & 1 << 11, 1 << 11, "TCIF1");
    b.write(DMA1 + 0x08, 1 << 11);
    // the buffer not in use may be changed while running; the other not
    b.write(cr + 0x0C, RAM + 0xC0);
    b.write(cr + 0x10, RAM + 0x20);
    assert_eq!(b.read(cr + 0x10), RAM + 0x80);
    b.ram.write(RAM, 4, 0x9ABC_DEF0);
    b.updates(3);
    assert_eq!(b.ram.read(RAM + 0x40, 4), Some(0x1234_5678));
    assert_eq!(b.ram.read(RAM + 0x44, 4), Some(0x1234_5678));
    assert_eq!(b.ram.read(RAM + 0x80, 4), Some(0x9ABC_DEF0));
    assert_eq!(b.ram.read(RAM + 0x84, 4), Some(0x9ABC_DEF0));
    assert_eq!(b.ram.read(RAM + 0xC0, 4), Some(0x9ABC_DEF0), "then the new M0AR");
    assert_eq!(b.ram.read(RAM + 0xC4, 4), Some(0));
    assert_eq!(b.read(cr) & (CR_CT | CR_EN), CR_EN, "still running, on M0AR");
}
