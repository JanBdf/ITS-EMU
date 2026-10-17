//! DMA1 and DMA2: eight streams each, in direct mode (no FIFO).
//!
//! Peripheral-to-memory, memory-to-peripheral (single transfers on the
//! requests of peripherals) and memory-to-memory (DMA2), with increments,
//! data sizes, circular mode, transfer complete/half/error flags and their
//! interrupts. Transfers take no time. The FIFO, bursts and double buffer
//! mode are not modelled.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Bus, BusMaster, Clock, DeviceCtx, Peripheral, Ram};

/// Interrupts of the streams.
const IRQS: [[u32; 8]; 2] = [[11, 12, 13, 14, 15, 16, 17, 47], [56, 57, 58, 59, 60, 68, 69, 70]];
const CR_EN: u32 = 1 << 0;
const CR_TEIE: u32 = 1 << 2;
const CR_HTIE: u32 = 1 << 3;
const CR_TCIE: u32 = 1 << 4;
const CR_CIRC: u32 = 1 << 8;
const CR_PINC: u32 = 1 << 9;
const CR_MINC: u32 = 1 << 10;
const TEIF: u32 = 1 << 3;
const HTIF: u32 = 1 << 4;
const TCIF: u32 = 1 << 5;

#[derive(Default, Clone, Copy)]
struct Stream {
    cr: u32,
    ndtr: u32,
    par: u32,
    m0ar: u32,
    m1ar: u32,
    fcr: u32,
    /// NDTR when enabled (reload in circular mode), current addresses.
    total: u32,
    p: u32,
    m: u32,
    /// Interrupt flags (TCIF, HTIF, TEIF, DMEIF, FEIF in their LISR positions
    /// for stream 0).
    flags: u32,
}

impl Stream {
    fn channel(&self) -> u32 {
        (self.cr >> 25) & 7
    }
    fn dir(&self) -> u32 {
        (self.cr >> 6) & 3
    }
    fn size(bits: u32) -> u32 {
        1 << bits.min(2)
    }
}

/// Bit offset of the flags of stream `s` in LISR/HISR.
fn flag_shift(s: usize) -> u32 {
    [0, 6, 16, 22][s % 4]
}

#[derive(Default)]
pub struct DmaState {
    streams: [[Stream; 8]; 2],
    enables: [bool; 2],
}

/// The DMA controllers and the request lines of the peripherals (kept apart:
/// a transfer reads or writes a peripheral, which then updates its request).
#[derive(Default)]
pub struct Dma {
    state: RefCell<DmaState>,
    /// Requests per controller and stream: bit n = channel n requests.
    requests: Cell<[[u8; 8]; 2]>,
    /// Streams that finished a (non-circular) transfer for channel n.
    ended: Cell<[[u8; 8]; 2]>,
}

pub type SharedDma = Rc<Dma>;

/// A request line of a peripheral: the streams (controller, stream, channel)
/// it can be served by.
#[derive(Clone, Copy)]
pub struct DmaLine(pub &'static [(usize, usize, u32)]);

impl DmaLine {
    pub fn set(&self, dma: &Dma, on: bool) {
        let mut r = dma.requests.get();
        for &(c, s, ch) in self.0 {
            if on {
                r[c][s] |= 1 << ch;
            } else {
                r[c][s] &= !(1 << ch);
            }
        }
        dma.requests.set(r);
    }

    /// Whether a stream serving this line finished its transfer since the
    /// last call (the end-of-transfer signal to the peripheral).
    pub fn take_end(&self, dma: &Dma) -> bool {
        let mut e = dma.ended.get();
        let mut any = false;
        for &(c, s, ch) in self.0 {
            any |= e[c][s] & (1 << ch) != 0;
            e[c][s] &= !(1 << ch);
        }
        dma.ended.set(e);
        any
    }
}

impl DmaState {
    fn sync_irqs(&self, ctx: &mut DeviceCtx) {
        for (streams, irqs) in self.streams.iter().zip(IRQS) {
            for (st, irq) in streams.iter().zip(irqs) {
                let on = (st.cr & CR_TCIE != 0 && st.flags & TCIF != 0)
                    || (st.cr & CR_HTIE != 0 && st.flags & HTIF != 0)
                    || (st.cr & CR_TEIE != 0 && st.flags & TEIF != 0);
                ctx.set_irq(irq, on);
            }
        }
    }

    /// Whether stream `s` of controller `c` has a transfer to make.
    fn due(&self, c: usize, s: usize, requests: &[[u8; 8]; 2]) -> bool {
        let st = &self.streams[c][s];
        st.cr & CR_EN != 0 && (st.dir() == 2 || requests[c][s] & (1 << st.channel()) != 0)
    }

    /// One item; returns false if the stream stopped.
    #[allow(clippy::too_many_arguments)]
    fn transfer(
        &mut self,
        c: usize,
        s: usize,
        ctx: &mut DeviceCtx,
        bus: &mut Bus,
        ram: &mut Ram,
        ended: &Cell<[[u8; 8]; 2]>,
    ) -> bool {
        let st = self.streams[c][s];
        let psize = Stream::size((st.cr >> 11) & 3);
        let msize = if st.fcr & 4 != 0 { Stream::size((st.cr >> 13) & 3) } else { psize };
        let periph = |a: u32| (0x4000_0000..0x6000_0000).contains(&a);
        let read = |addr: u32, size: u32, bus: &mut Bus, ctx: &mut DeviceCtx, ram: &Ram| {
            if periph(addr) { Some(bus.read(ctx, addr, size)) } else { ram.read(addr, size) }
        };
        let write = |addr: u32, size: u32, v: u32, bus: &mut Bus, ctx: &mut DeviceCtx, ram: &mut Ram| {
            if periph(addr) {
                bus.write(ctx, addr, size, v);
                true
            } else {
                ram.write(addr, size, v)
            }
        };
        // peripheral to memory and memory to memory read PAR, write M0AR
        let (src, ssize, dst, dsize) = match st.dir() {
            1 => (st.m, msize, st.p, psize),
            _ => (st.p, psize, st.m, msize),
        };
        let ok = match read(src, ssize, bus, ctx, ram) {
            Some(v) => write(dst, dsize, v, bus, ctx, ram),
            None => false,
        };
        let st = &mut self.streams[c][s];
        if !ok {
            st.flags |= TEIF;
            st.cr &= !CR_EN;
            return false;
        }
        if st.cr & CR_PINC != 0 {
            st.p = st.p.wrapping_add(psize);
        }
        if st.cr & CR_MINC != 0 {
            st.m = st.m.wrapping_add(msize);
        }
        st.ndtr -= 1;
        if st.ndtr == st.total / 2 {
            st.flags |= HTIF;
        }
        if st.ndtr == 0 {
            st.flags |= TCIF;
            if st.cr & CR_CIRC != 0 {
                (st.ndtr, st.p, st.m) = (st.total, st.par, st.m0ar);
            } else {
                st.cr &= !CR_EN;
                let ch = st.channel();
                let mut e = ended.get();
                e[c][s] |= 1 << ch;
                ended.set(e);
                return false;
            }
        }
        true
    }
}

/// The DMA controllers as a bus master.
pub struct DmaMaster(pub SharedDma);

impl BusMaster for DmaMaster {
    fn wants_service(&self) -> bool {
        let d = self.0.state.borrow();
        let r = self.0.requests.get();
        (0..2).any(|c| d.enables[c] && (0..8).any(|s| d.due(c, s, &r)))
    }

    fn service(&mut self, ctx: &mut DeviceCtx, bus: &mut Bus, ram: &mut Ram) {
        let mut d = self.0.state.borrow_mut();
        for c in 0..2 {
            for s in 0..8 {
                if !d.due(c, s, &self.0.requests.get()) {
                    continue;
                }
                // memory to memory runs to the end at once; a request gets one item
                if d.streams[c][s].dir() == 2 {
                    while d.transfer(c, s, ctx, bus, ram, &self.0.ended) {}
                } else {
                    d.transfer(c, s, ctx, bus, ram, &self.0.ended);
                }
            }
        }
        d.sync_irqs(ctx);
    }
}

/// Register interface of DMA1 (`index` 0) or DMA2.
pub struct DmaRegs {
    index: usize,
    state: SharedDma,
    clocks: crate::rcc::SharedClocks,
}

impl DmaRegs {
    pub fn new(index: usize, state: SharedDma, clocks: crate::rcc::SharedClocks) -> Self {
        Self { index, state, clocks }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().ahb1enr & (1 << (21 + self.index)) != 0
    }

    fn isr(&self, high: bool) -> u32 {
        let d = self.state.state.borrow();
        let base = if high { 4 } else { 0 };
        (0..4).fold(0, |acc, i| acc | (d.streams[self.index][base + i].flags & 0x3D) << flag_shift(i))
    }
}

impl Peripheral for DmaRegs {
    fn name(&self) -> &str {
        if self.index == 0 { "DMA1" } else { "DMA2" }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let reg = off & !3;
        let v = match reg {
            0x00 => self.isr(false),
            0x04 => self.isr(true),
            0x10..=0xCC => {
                let s = (reg as usize - 0x10) / 0x18;
                let st = &self.state.state.borrow().streams[self.index][s];
                match (reg - 0x10) % 0x18 {
                    0x00 => st.cr,
                    0x04 => st.ndtr,
                    0x08 => st.par,
                    0x0C => st.m0ar,
                    0x10 => st.m1ar,
                    _ => st.fcr | 0x20, // FS: FIFO empty
                }
            }
            _ => 0,
        };
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        let reg = off & !3;
        let mut d = self.state.state.borrow_mut();
        d.enables[self.index] = true;
        match reg {
            0x08 | 0x0C => {
                let base = if reg == 0x0C { 4 } else { 0 };
                for i in 0..4 {
                    let clear = (value >> flag_shift(i)) & 0x3D;
                    d.streams[self.index][base + i].flags &= !clear;
                }
            }
            0x10..=0xCC => {
                let s = (reg as usize - 0x10) / 0x18;
                let st = &mut d.streams[self.index][s];
                let field = (reg - 0x10) % 0x18;
                let enabled = st.cr & CR_EN != 0;
                match field {
                    0x00 => {
                        let v = merge_write(st.cr, off, size, value) & 0x0FEF_FFFF;
                        if !enabled && v & CR_EN != 0 {
                            // latch the transfer
                            (st.total, st.p, st.m) = (st.ndtr, st.par, st.m0ar);
                            if st.ndtr == 0 {
                                st.flags |= TEIF;
                                st.cr = v & !CR_EN;
                                return;
                            }
                        }
                        st.cr = v;
                    }
                    // the others are read-only while the stream is enabled
                    _ if enabled => {}
                    0x04 => st.ndtr = value & 0xFFFF,
                    0x08 => st.par = value,
                    0x0C => st.m0ar = value,
                    0x10 => st.m1ar = value,
                    _ => st.fcr = value & 0x87,
                }
            }
            _ => {}
        }
        d.sync_irqs(ctx);
    }

    fn reset(&mut self) {
        self.state.state.borrow_mut().streams[self.index] = [Stream { fcr: 1, ..Default::default() }; 8];
    }
}
