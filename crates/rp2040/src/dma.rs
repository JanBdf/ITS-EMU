//! DMA: twelve channels with chaining, rings, the alias register layouts
//! (control blocks), the pacing timers and the interrupt lines.
//!
//! The registers are a [`Block`] on the shared bus; the transfers are made
//! by [`pump`], which the shared bus runs around every access and update:
//! unpaced channels (permanent DREQ) and channels paced by a pacing timer
//! move one item per cycle (or at the timer's rate) from their trigger on;
//! channels paced by a peripheral (ADC, UART) move an item whenever its
//! register says it has data or room, channels paced by a PWM slice one
//! item per wrap (the PWM raises the DREQ while such a channel is busy).
//!
//! The PIOs work with channels paced by their FIFOs directly: a state
//! machine that pulls takes the next word from the channel (the TX FIFO is
//! kept full, as the DMA does), one that pushes hands it over, so the state
//! machines run as on the chip, and they report when such a channel can
//! end at the earliest (its interrupt is on time).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use emu_core::{Bus, Clock, DeviceCtx, Ram};

use crate::regs::{Block, Notices, Op};

/// System interrupt lines DMA_IRQ_0 and DMA_IRQ_1.
pub const DMA_IRQ_0: u32 = 11;
pub const CHANNELS: usize = 12;

/// TREQ_SEL values.
pub const TREQ_PIO0_TX0: u8 = 0x00;
pub const TREQ_UART0_TX: u8 = 0x14;
pub const TREQ_UART1_RX: u8 = 0x17;
pub const TREQ_PWM_WRAP0: u8 = 0x18;
pub const TREQ_PWM_WRAP7: u8 = 0x1F;
pub const TREQ_ADC: u8 = 0x24;
pub const TREQ_TIMER0: u8 = 0x3B;
pub const TREQ_PERMANENT: u8 = 0x3F;

const PIO_BASE: [u32; 2] = [0x5020_0000, 0x5030_0000];
const ADC_FCS: u32 = 0x4004_C008;
const UART_FR: [u32; 2] = [0x4003_4018, 0x4003_8018];

const CTRL_EN: u32 = 1 << 0;
const CTRL_INCR_READ: u32 = 1 << 4;
const CTRL_INCR_WRITE: u32 = 1 << 5;
const CTRL_RING_SEL: u32 = 1 << 10;
const CTRL_IRQ_QUIET: u32 = 1 << 21;
const CTRL_BSWAP: u32 = 1 << 22;
const CTRL_BUSY: u32 = 1 << 24;
/// What software can write (BUSY and the error flags are read-only).
const CTRL_WRITABLE: u32 = 0x00FF_FFFF;

#[derive(Clone, Debug, Default)]
struct Channel {
    read_addr: u32,
    write_addr: u32,
    /// Transfers left while busy.
    count: u32,
    /// TRANS_COUNT as written: loaded into `count` at each trigger.
    reload: u32,
    ctrl: u32,
    busy: bool,
    /// Time-paced channels: the cycle the pace was set from and the
    /// transfers made since.
    paced_from: u64,
    paced_done: u64,
}

impl Channel {
    fn treq(&self) -> u8 {
        ((self.ctrl >> 15) & 0x3F) as u8
    }

    fn size(&self) -> u32 {
        1 << ((self.ctrl >> 2) & 3).min(2)
    }

    fn chain_to(&self) -> usize {
        ((self.ctrl >> 11) & 0xF) as usize
    }

    fn running(&self) -> bool {
        self.busy && self.ctrl & CTRL_EN != 0
    }

    /// Advances an address by one item, wrapping within the ring when the
    /// ring is on that side.
    fn step(&self, addr: u32, write: bool) -> u32 {
        let incr = if write { CTRL_INCR_WRITE } else { CTRL_INCR_READ };
        if self.ctrl & incr == 0 {
            return addr;
        }
        let next = addr.wrapping_add(self.size());
        let ring = (self.ctrl >> 6) & 0xF;
        if ring == 0 || (self.ctrl & CTRL_RING_SEL != 0) != write {
            return next;
        }
        let mask = (1u32 << ring) - 1;
        (addr & !mask) | (next & mask)
    }

    /// The value as the channel writes it: byte-swapped within the item
    /// with BSWAP, replicated over the word as narrow bus writes are.
    fn data(&self, v: u32) -> u32 {
        let size = self.size();
        let swapped = self.ctrl & CTRL_BSWAP != 0;
        match size {
            1 => (v & 0xFF) * 0x0101_0101,
            2 => {
                let h = v as u16;
                u32::from(if swapped { h.swap_bytes() } else { h }) * 0x0001_0001
            }
            _ => {
                if swapped {
                    v.swap_bytes()
                } else {
                    v
                }
            }
        }
    }
}

/// The wrap DREQs of the PWM slices: the slices a busy channel waits on
/// (since which cycle), and the wraps the DMA has not taken yet.
#[derive(Clone, Copy, Default)]
pub struct PwmDreq {
    pub wanted: u8,
    pub since: [u64; 8],
    pub pending: u8,
}

pub type SharedPwmDreq = Rc<Cell<PwmDreq>>;

/// How a channel's transfers are paced.
enum Pace {
    /// Items per cycle: x / y (permanent DREQ: one per cycle).
    Rate(u64, u64),
    /// A PIO FIFO (fed by the PIO, or through the bus).
    Pio,
    /// A peripheral's DREQ, read from its registers.
    Peripheral,
    /// Not emulated (SPI, I2C, XIP).
    Never,
}

pub struct DmaCore {
    ch: [Channel; CHANNELS],
    intr: u32,
    inte: [u32; 2],
    intf: [u32; 2],
    timers: [u32; 4],
    sniff_ctrl: u32,
    sniff_data: u32,
    /// The cores' memory, for transfers from and to it.
    ram: Option<Ram>,
    notices: Notices,
    /// What was noted to the user already.
    noted: Vec<String>,
    /// While the pump writes a register: the cycle that transfer was due
    /// (a trigger it writes starts the channel then).
    transfer_at: Option<u64>,
    /// The busy channels (a cheap test for the pump around every access).
    busy: u16,
    pwm: SharedPwmDreq,
}

pub type SharedDma = Rc<RefCell<DmaCore>>;

pub fn shared(notices: Notices) -> SharedDma {
    Rc::new(RefCell::new(DmaCore {
        ch: Default::default(),
        intr: 0,
        inte: [0; 2],
        intf: [0; 2],
        timers: [0; 4],
        sniff_ctrl: 0,
        sniff_data: 0,
        ram: None,
        notices,
        noted: Vec::new(),
        transfer_at: None,
        busy: 0,
        pwm: SharedPwmDreq::default(),
    }))
}

/// The memory the DMA reads and writes directly; flash through its aliases.
fn memory_addr(addr: u32) -> u32 {
    match addr >> 24 {
        0x11..=0x13 => 0x1000_0000 | (addr & 0x00FF_FFFF),
        _ => addr,
    }
}

impl DmaCore {
    /// Gives the DMA the cores' memory (once the cores exist).
    pub fn set_memory(&mut self, ram: Ram) {
        self.ram = Some(ram);
    }

    /// The PWM's wrap DREQs (the PWM raises them, the DMA takes them).
    pub fn pwm_dreq(&self) -> SharedPwmDreq {
        self.pwm.clone()
    }

    /// Tells the PWM which slices busy channels wait on (from `now`).
    fn sync_pwm_wanted(&self, now: u64) {
        let mut p = self.pwm.get();
        let wanted = (0..CHANNELS)
            .filter(|&n| self.ch[n].busy)
            .map(|n| self.ch[n].treq())
            .filter(|t| (TREQ_PWM_WRAP0..=TREQ_PWM_WRAP7).contains(t))
            .fold(0u8, |acc, t| acc | 1 << (t - TREQ_PWM_WRAP0));
        if wanted == p.wanted {
            return;
        }
        for i in 0..8 {
            if wanted & !p.wanted & (1 << i) != 0 {
                p.since[i] = now;
            }
        }
        p.pending &= wanted;
        p.wanted = wanted;
        self.pwm.set(p);
    }

    fn note(&mut self, text: String) {
        if !self.noted.contains(&text) {
            self.noted.push(text.clone());
            self.notices.note(text);
        }
    }

    fn pace(&self, n: usize) -> Pace {
        let treq = self.ch[n].treq();
        match treq {
            0x00..=0x0F => Pace::Pio,
            TREQ_UART0_TX..=TREQ_PWM_WRAP7 | TREQ_ADC => Pace::Peripheral,
            TREQ_TIMER0..=0x3E => {
                let t = self.timers[usize::from(treq - TREQ_TIMER0)];
                let (x, y) = (u64::from(t >> 16), u64::from(t & 0xFFFF));
                if x == 0 || y == 0 { Pace::Never } else { Pace::Rate(x, y) }
            }
            TREQ_PERMANENT => Pace::Rate(1, 1),
            _ => Pace::Never,
        }
    }

    /// When the next transfer of time-paced channel `n` is due.
    fn due_at(&self, n: usize) -> Option<u64> {
        let Pace::Rate(x, y) = self.pace(n) else { return None };
        let c = &self.ch[n];
        Some(c.paced_from + (u128::from(c.paced_done + 1) * u128::from(y)).div_ceil(u128::from(x)) as u64)
    }

    /// The pace of channel `n` starts over at `now` (trigger, enable, timer).
    fn repace(&mut self, n: usize, now: u64) {
        let c = &mut self.ch[n];
        c.paced_from = now;
        c.paced_done = 0;
    }

    /// Starts channel `n` at cycle `at`.
    fn trigger(&mut self, ctx: &mut DeviceCtx, n: usize, at: u64) {
        let c = &mut self.ch[n];
        if c.ctrl & CTRL_EN == 0 || c.busy {
            return;
        }
        c.count = c.reload;
        if c.count == 0 {
            // nothing to move: done at once
            self.complete(ctx, n, at);
            return;
        }
        c.busy = true;
        self.busy |= 1 << n;
        if matches!(self.pace(n), Pace::Never) {
            let treq = self.ch[n].treq();
            self.note(format!("DMA paced by DREQ {treq:#04x} (SPI, I2C or XIP), which is not emulated"));
        }
        self.repace(n, at);
    }

    /// A trigger register written: a value of zero is a null trigger (the
    /// end of a control block list), which only raises the interrupt of a
    /// channel with IRQ_QUIET.
    fn write_trigger(&mut self, ctx: &mut DeviceCtx, n: usize, value: u32) {
        if value == 0 {
            if self.ch[n].ctrl & CTRL_IRQ_QUIET != 0 {
                self.intr |= 1 << n;
                self.sync_irq(ctx);
            }
            return;
        }
        let at = self.transfer_at.unwrap_or(ctx.now());
        self.trigger(ctx, n, at);
    }

    /// Channel `n` made its last transfer at cycle `at`; a channel it
    /// chains to starts then.
    fn complete(&mut self, ctx: &mut DeviceCtx, n: usize, at: u64) {
        let c = &mut self.ch[n];
        c.busy = false;
        self.busy &= !(1 << n);
        if c.ctrl & CTRL_IRQ_QUIET == 0 {
            self.intr |= 1 << n;
        }
        let chain = c.chain_to();
        if chain != n && chain < CHANNELS {
            self.trigger(ctx, chain, at);
        }
        self.sync_irq(ctx);
    }

    /// One item of channel `n` moved at cycle `at`: addresses and count advance.
    fn transferred(&mut self, ctx: &mut DeviceCtx, n: usize, at: u64) {
        let c = &mut self.ch[n];
        if !c.busy {
            return;
        }
        c.read_addr = c.step(c.read_addr, false);
        c.write_addr = c.step(c.write_addr, true);
        c.count = c.count.saturating_sub(1);
        c.paced_done += 1;
        if c.count == 0 {
            self.complete(ctx, n, at);
        }
    }

    fn ints(&self, line: usize) -> u32 {
        (self.intr | self.intf[line]) & self.inte[line]
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        for line in 0..2 {
            ctx.set_irq(DMA_IRQ_0 + line as u32, self.ints(line) != 0);
        }
    }

    /// Whether a channel is busy (it may be paused).
    pub fn any_busy(&self) -> bool {
        self.busy != 0
    }

    /// The running channel paced by DREQ `dreq` (0..7) of PIO `pio`.
    fn pio_channel(&self, pio: usize, dreq: u8) -> Option<usize> {
        let treq = (pio as u8) << 3 | dreq;
        (0..CHANNELS).find(|&n| self.ch[n].running() && self.ch[n].treq() == treq)
    }

    fn mem_read(&self, addr: u32, size: u32) -> Option<u32> {
        self.ram.as_ref()?.read(memory_addr(addr), size)
    }

    /// The next word for the TX FIFO of state machine `sm` of PIO `pio`
    /// from a channel paced by it that reads memory and writes that FIFO
    /// (the state machine pulls at cycle `at`).
    pub fn feed_tx(&mut self, ctx: &mut DeviceCtx, pio: usize, sm: usize, at: u64) -> Option<u32> {
        let n = self.pio_channel(pio, sm as u8)?;
        let c = &self.ch[n];
        if c.write_addr & !0x3000 != PIO_BASE[pio] + 0x10 + 4 * sm as u32 {
            return None;
        }
        let v = self.mem_read(c.read_addr, c.size())?;
        let v = c.data(v);
        self.transferred(ctx, n, at);
        Some(v)
    }

    /// Takes `value` from the RX FIFO of state machine `sm` of PIO `pio`
    /// for a channel paced by it that reads that FIFO and writes memory
    /// (pushed at cycle `at`).
    pub fn take_rx(&mut self, ctx: &mut DeviceCtx, pio: usize, sm: usize, value: u32, at: u64) -> bool {
        let Some(n) = self.pio_channel(pio, 4 + sm as u8) else { return false };
        let c = &self.ch[n];
        if c.read_addr & !0x3000 != PIO_BASE[pio] + 0x20 + 4 * sm as u32 {
            return false;
        }
        let (addr, size, v) = (memory_addr(c.write_addr), c.size(), c.data(value));
        let Some(ram) = self.ram.as_mut() else { return false };
        if !ram.write(addr, size, v) {
            return false;
        }
        self.transferred(ctx, n, at);
        true
    }

    /// Transfers left of the running channel paced by DREQ `dreq` of PIO
    /// `pio` (the PIO's earliest end of it).
    pub fn pio_remaining(&self, pio: usize, dreq: u8) -> Option<u32> {
        self.pio_channel(pio, dreq).map(|n| self.ch[n].count)
    }

    /// The next transfer to make by `now`: the channel, the cycle it is
    /// due at and whether a peripheral's DREQ is to be checked first
    /// (`skip`: channels whose DREQ is not asserted now). Time-paced
    /// transfers go in the order they are due.
    fn next_job(&self, now: u64, skip: u16) -> Option<(usize, u64, bool)> {
        let running = (0..CHANNELS).filter(|&n| self.ch[n].running() && skip & (1 << n) == 0);
        let timed = running.clone().filter_map(|n| Some((n, self.due_at(n)?))).filter(|&(_, at)| at <= now);
        if let Some((n, at)) = timed.min_by_key(|&(n, at)| (at, n)) {
            return Some((n, at, false));
        }
        running.filter(|&n| matches!(self.pace(n), Pace::Pio | Pace::Peripheral)).map(|n| (n, now, true)).next()
    }
}

/// Whether the peripheral DREQ that paces channel `treq` is asserted.
fn dreq(bus: &mut Bus, ctx: &mut DeviceCtx, treq: u8, pwm: &PwmDreq) -> bool {
    match treq {
        TREQ_PWM_WRAP0..=TREQ_PWM_WRAP7 => pwm.pending & (1 << (treq - TREQ_PWM_WRAP0)) != 0,
        0x00..=0x0F => {
            let (pio, dreq) = (usize::from(treq >> 3), treq & 7);
            let fstat = bus.read(ctx, PIO_BASE[pio] + 0x4, 4);
            if dreq < 4 {
                fstat & (1 << (16 + dreq)) == 0 // TX not full
            } else {
                fstat & (1 << (8 + dreq - 4)) == 0 // RX not empty
            }
        }
        TREQ_UART0_TX..=TREQ_UART1_RX => {
            let uart = usize::from((treq - TREQ_UART0_TX) / 2);
            let fr = bus.read(ctx, UART_FR[uart], 4);
            if treq & 1 == 0 { fr & (1 << 5) == 0 } else { fr & (1 << 4) == 0 }
        }
        TREQ_ADC => {
            let fcs = bus.read(ctx, ADC_FCS, 4);
            let (level, thresh) = ((fcs >> 16) & 0xF, ((fcs >> 24) & 0xF).max(1));
            fcs & (1 << 3) != 0 && level >= thresh
        }
        _ => false,
    }
}

/// Makes the transfers that are due (see the module comment); returns
/// whether anything happened.
pub fn pump(dma: &SharedDma, bus: &mut Bus, ctx: &mut DeviceCtx) -> bool {
    if !dma.borrow().any_busy() {
        return false;
    }
    let ram = dma.borrow().ram.clone();
    let mut skip = 0u16;
    let mut moved = false;
    // bounded: a channel may keep re-triggering itself through its chain
    for _ in 0..1 << 20 {
        let now = ctx.now();
        let Some((n, at, check)) = dma.borrow().next_job(now, skip) else { break };
        let (treq, pwm) = {
            let d = dma.borrow();
            d.sync_pwm_wanted(now);
            (d.ch[n].treq(), d.pwm.get())
        };
        if check && !dreq(bus, ctx, treq, &pwm) {
            skip |= 1 << n;
            continue;
        }
        if (TREQ_PWM_WRAP0..=TREQ_PWM_WRAP7).contains(&treq) {
            // the transfer takes the wrap's request
            let d = dma.borrow();
            let mut p = d.pwm.get();
            p.pending &= !(1 << (treq - TREQ_PWM_WRAP0));
            d.pwm.set(p);
        }
        let (read_addr, write_addr, size) = {
            let d = dma.borrow();
            let c = &d.ch[n];
            if !c.running() {
                continue;
            }
            (c.read_addr, c.write_addr, c.size())
        };
        // the bus accesses may reach the DMA's own registers (control blocks)
        let v = match ram.as_ref().and_then(|r| r.read(memory_addr(read_addr), size)) {
            Some(v) => v,
            None => bus.read(ctx, read_addr, size),
        };
        let v = dma.borrow().ch[n].data(v);
        let in_memory = ram.as_ref().is_some_and(|r| r.read(memory_addr(write_addr), size).is_some());
        if in_memory {
            if let Some(r) = dma.borrow_mut().ram.as_mut() {
                r.write(memory_addr(write_addr), size, v);
            }
        } else {
            dma.borrow_mut().transfer_at = Some(at);
            bus.write(ctx, write_addr, size, v);
            dma.borrow_mut().transfer_at = None;
        }
        dma.borrow_mut().transferred(ctx, n, at);
        moved = true;
    }
    dma.borrow().sync_pwm_wanted(ctx.now());
    moved
}

/// The DMA's registers.
pub struct Dma(pub SharedDma);

impl Block for Dma {
    fn name(&self) -> &str {
        "DMA"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        let d = self.0.borrow();
        match off {
            0x000..=0x2FF => {
                let c = &d.ch[(off / 0x40) as usize];
                let ctrl = (c.ctrl & CTRL_WRITABLE) | if c.busy { CTRL_BUSY } else { 0 };
                match off % 0x40 {
                    0x00 | 0x14 | 0x28 | 0x3C => c.read_addr,
                    0x04 | 0x18 | 0x2C | 0x34 => c.write_addr,
                    0x08 | 0x1C | 0x24 | 0x38 => c.count,
                    _ => ctrl,
                }
            }
            0x400 => d.intr,
            0x404 => d.inte[0],
            0x408 => d.intf[0],
            0x40C => d.ints(0),
            0x414 => d.inte[1],
            0x418 => d.intf[1],
            0x41C => d.ints(1),
            0x420..=0x42C => d.timers[((off - 0x420) / 4) as usize],
            0x434 => d.sniff_ctrl,
            0x438 => d.sniff_data,
            0x448 => CHANNELS as u32,
            0x800..=0xAFF if off % 0x40 == 4 => d.ch[((off - 0x800) / 0x40) as usize].reload,
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        let mut d = self.0.borrow_mut();
        let d = &mut *d;
        match off {
            0x000..=0x2FF => {
                let n = (off / 0x40) as usize;
                let reg = off % 0x40;
                let c = &mut d.ch[n];
                match reg {
                    0x00 | 0x14 | 0x28 | 0x3C => c.read_addr = op.apply(c.read_addr, value),
                    0x04 | 0x18 | 0x2C | 0x34 => c.write_addr = op.apply(c.write_addr, value),
                    0x08 | 0x1C | 0x24 | 0x38 => c.reload = op.apply(c.reload, value),
                    _ => {
                        let was = c.ctrl & CTRL_EN;
                        c.ctrl = op.apply(c.ctrl, value) & CTRL_WRITABLE;
                        if c.ctrl & (1 << 23) != 0 {
                            d.note(
                                "The firmware uses the DMA sniffer (CRC of transferred data), which is not emulated"
                                    .into(),
                            );
                        }
                        if was == 0 && d.ch[n].ctrl & CTRL_EN != 0 && d.ch[n].busy {
                            // a paused channel goes on from now
                            d.repace(n, ctx.now());
                        }
                    }
                }
                // CTRL_TRIG, AL1_TRANS_COUNT_TRIG, AL2_WRITE_ADDR_TRIG, AL3_READ_ADDR_TRIG
                if reg % 0x10 == 0xC {
                    let written = if reg == 0xC { 1 } else { value };
                    d.write_trigger(ctx, n, written);
                }
            }
            0x400 => d.intr &= !op.clears(value),
            0x404 => d.inte[0] = op.apply(d.inte[0], value) & 0xFFF,
            0x408 => d.intf[0] = op.apply(d.intf[0], value) & 0xFFF,
            0x40C | 0x41C => d.intr &= !op.clears(value),
            0x414 => d.inte[1] = op.apply(d.inte[1], value) & 0xFFF,
            0x418 => d.intf[1] = op.apply(d.intf[1], value) & 0xFFF,
            0x420..=0x42C => {
                let t = ((off - 0x420) / 4) as usize;
                d.timers[t] = op.apply(d.timers[t], value);
                let treq = TREQ_TIMER0 + t as u8;
                for n in 0..CHANNELS {
                    if d.ch[n].treq() == treq {
                        d.repace(n, ctx.now());
                    }
                }
            }
            0x430 => {
                let bits = op.apply(0, value) & 0xFFF;
                for n in (0..CHANNELS).filter(|n| bits & (1 << n) != 0) {
                    d.trigger(ctx, n, ctx.now());
                }
            }
            0x434 => d.sniff_ctrl = op.apply(d.sniff_ctrl, value),
            0x438 => d.sniff_data = op.apply(d.sniff_data, value),
            0x444 => {
                // CHAN_ABORT: the channels stop without an interrupt
                let bits = op.apply(0, value) & 0xFFF;
                for n in (0..CHANNELS).filter(|n| bits & (1 << n) != 0) {
                    d.ch[n].busy = false;
                    d.busy &= !(1 << n);
                }
            }
            _ => {}
        }
        d.sync_pwm_wanted(ctx.now());
        d.sync_irq(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        // the end of time-paced channels (their interrupt and BUSY)
        let d = self.0.borrow();
        (0..CHANNELS)
            .filter(|&n| d.ch[n].running())
            .filter_map(|n| match d.pace(n) {
                Pace::Rate(x, y) => {
                    let c = &d.ch[n];
                    let total = c.paced_done + u64::from(c.count);
                    Some(c.paced_from + (u128::from(total) * u128::from(y)).div_ceil(u128::from(x)) as u64)
                }
                _ => None,
            })
            .min()
    }

    fn read_stable_until(&self, _off: u32, clock: &Clock) -> u64 {
        if self.0.borrow().any_busy() { clock.cycles() } else { u64::MAX }
    }

    fn reset(&mut self) {
        let mut d = self.0.borrow_mut();
        let (ram, notices, noted) = (d.ram.take(), d.notices.clone(), std::mem::take(&mut d.noted));
        let pwm = d.pwm.clone();
        pwm.set(PwmDreq::default());
        *d = DmaCore {
            ch: Default::default(),
            intr: 0,
            inte: [0; 2],
            intf: [0; 2],
            timers: [0; 4],
            sniff_ctrl: 0,
            sniff_data: 0,
            ram,
            notices,
            noted,
            transfer_at: None,
            busy: 0,
            pwm,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{Aliased, BLOCK_WINDOW};
    use emu_core::nvic::Nvic;

    const RAM: u32 = 0x2000_0000;
    const CH: u32 = 0x5000_0000;
    const PERMANENT: u32 = 0x3F << 15;
    const WORD: u32 = 2 << 2;

    struct Rig {
        bus: Bus,
        dma: SharedDma,
        clock: Clock,
        nvic: Nvic,
        _mem: Box<[u8]>,
    }

    impl Rig {
        fn new() -> Self {
            let dma = shared(Notices::default());
            let mut bus = Bus::new();
            bus.attach(CH, BLOCK_WINDOW, Box::new(Aliased(Dma(dma.clone()))));
            let mut mem = vec![0u8; 0x1000].into_boxed_slice();
            let mut ram = Ram::default();
            // SAFETY: the rig keeps the memory as long as the DMA
            unsafe { ram.add(RAM, mem.as_mut_ptr(), 0x1000, true) };
            dma.borrow_mut().set_memory(ram);
            Self { bus, dma, clock: Clock::new(125_000_000), nvic: Nvic::new(), _mem: mem }
        }

        fn ctx(&mut self) -> (DeviceCtx<'_>, &mut Bus, &SharedDma) {
            (DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic }, &mut self.bus, &self.dma)
        }

        fn write(&mut self, addr: u32, v: u32) {
            let (mut ctx, bus, dma) = self.ctx();
            if !(RAM..RAM + 0x1000).contains(&addr) {
                bus.write(&mut ctx, addr, 4, v);
            } else {
                dma.borrow_mut().ram.as_mut().unwrap().write(addr, 4, v);
            }
            pump(dma, bus, &mut ctx);
        }

        fn read(&mut self, addr: u32) -> u32 {
            let (mut ctx, bus, dma) = self.ctx();
            if (RAM..RAM + 0x1000).contains(&addr) {
                dma.borrow().mem_read(addr, 4).unwrap()
            } else {
                bus.read(&mut ctx, addr, 4)
            }
        }

        /// Runs `cycles`, stopping at the DMA's events as the machine does.
        fn run(&mut self, cycles: u64) {
            let end = self.clock.cycles() + cycles;
            loop {
                let now = self.clock.cycles();
                let to = self.next_event().filter(|&t| t > now).map_or(end, |t| t.min(end));
                self.clock.advance(to - now);
                let (mut ctx, bus, dma) = self.ctx();
                bus.update(&mut ctx);
                pump(dma, bus, &mut ctx);
                if to == end {
                    break;
                }
            }
        }

        fn next_event(&self) -> Option<u64> {
            self.bus.next_event()
        }
    }

    #[test]
    fn a_pacing_timer_sets_the_rate_and_the_end_is_an_event() {
        let mut r = Rig::new();
        for i in 0..4 {
            r.write(RAM + 4 * i, 10 + i);
        }
        r.write(0x5000_0420, (1 << 16) | 100); // TIMER0: one transfer per 100 cycles
        r.write(0x5000_0404, 1); // INTE0
        r.write(CH, RAM);
        r.write(CH + 4, RAM + 0x100);
        r.write(CH + 8, 4);
        let start = r.clock.cycles();
        r.write(CH + 0xC, 1 | WORD | CTRL_INCR_READ | CTRL_INCR_WRITE | u32::from(TREQ_TIMER0) << 15);
        assert_eq!(r.next_event(), Some(start + 400));
        r.run(250);
        assert_eq!(r.read(CH + 8), 2, "two transfers made");
        assert_eq!(r.read(RAM + 0x104), 11);
        assert_eq!(r.read(RAM + 0x108), 0, "the third is not due yet");
        assert!(!r.nvic.irq_level(DMA_IRQ_0));
        r.run(150);
        assert_eq!(r.read(CH + 0xC) & CTRL_BUSY, 0);
        assert_eq!(r.read(RAM + 0x10C), 13);
        assert_eq!(r.read(0x5000_040C), 1, "INTS0");
        assert!(r.nvic.irq_level(DMA_IRQ_0));
        r.write(0x5000_040C, 1);
        assert!(!r.nvic.irq_level(DMA_IRQ_0), "cleared through INTS0");
        assert_eq!(r.next_event(), None);
    }

    #[test]
    fn pwm_wraps_pace_a_channel() {
        const PWM: u32 = 0x4005_0000;
        let mut r = Rig::new();
        let pwm = crate::pwm::Pwm::new(crate::pins::SharedPins::default(), Notices::default());
        let dreq = r.dma.borrow().pwm_dreq();
        r.bus.attach(PWM, BLOCK_WINDOW, Box::new(Aliased(pwm.with_dreq(dreq))));
        for i in 0..4 {
            r.write(RAM + 4 * i, 10 * (i + 1));
        }
        r.write(PWM + 0x10, 99); // TOP: a wrap every 100 cycles
        r.write(PWM, 1); // EN
        r.run(150); // a wrap before the channel starts is no request
        // CC of slice 0, one value per wrap
        r.write(CH, RAM);
        r.write(CH + 4, PWM + 0xC);
        r.write(CH + 8, 4);
        r.write(CH + 0xC, 1 | WORD | CTRL_INCR_READ | u32::from(TREQ_PWM_WRAP0) << 15);
        assert_eq!(r.read(PWM + 0xC), 0, "waits for the wrap");
        r.run(100); // the wrap at 200
        assert_eq!((r.read(PWM + 0xC), r.read(CH + 8)), (10, 3));
        r.run(250); // 300, 400, 500
        assert_eq!(r.read(PWM + 0xC), 40);
        assert_eq!(r.read(CH + 0xC) & CTRL_BUSY, 0);
        assert_eq!(r.dma.borrow().pwm_dreq().get().wanted, 0, "no more DREQs wanted");
    }

    #[test]
    fn a_ring_wraps_the_write_address() {
        let mut r = Rig::new();
        for i in 0..4 {
            r.write(RAM + 4 * i, 20 + i);
        }
        r.write(CH, RAM);
        r.write(CH + 4, RAM + 0x100);
        r.write(CH + 8, 4);
        let ring = CTRL_RING_SEL | 3 << 6; // 8 bytes on the write side
        r.write(CH + 0xC, 1 | WORD | CTRL_INCR_READ | CTRL_INCR_WRITE | ring | PERMANENT);
        r.run(10);
        assert_eq!([r.read(RAM + 0x100), r.read(RAM + 0x104), r.read(RAM + 0x108)], [22, 23, 0]);
        assert_eq!(r.read(CH + 4), RAM + 0x100, "back at the start of the ring");
    }

    /// The SDK's control-block pattern: channel 0 writes (count, source)
    /// pairs from a list into channel 1's alias registers, the second of
    /// them a trigger; channel 1 chains back to channel 0; a pair of zeros
    /// ends the list with a null trigger.
    #[test]
    fn control_blocks_reprogram_a_channel_until_a_null_trigger() {
        let mut r = Rig::new();
        let (a, b) = (RAM + 0x200, RAM + 0x300);
        for i in 0..3 {
            r.write(a + 4 * i, 0xA0 + i);
            r.write(b + 4 * i, 0xB0 + i);
        }
        let list = RAM + 0x400;
        for (i, v) in [2, a, 3, b, 0, 0].into_iter().enumerate() {
            r.write(list + 4 * i as u32, v);
        }
        // channel 1: copies to 0x800 on, chains to 0, interrupt only at a null trigger
        r.write(CH + 0x40 + 4, RAM + 0x800);
        r.write(CH + 0x40 + 0x10, 1 | WORD | CTRL_INCR_READ | CTRL_INCR_WRITE | CTRL_IRQ_QUIET | PERMANENT);
        // channel 0: two words per block into AL3_TRANS_COUNT, AL3_READ_ADDR_TRIG
        r.write(CH, list);
        r.write(CH + 4, CH + 0x40 + 0x38);
        r.write(CH + 8, 2);
        let ring = CTRL_RING_SEL | 3 << 6;
        r.write(CH + 0xC, 1 | WORD | CTRL_INCR_READ | CTRL_INCR_WRITE | ring | PERMANENT);
        r.run(100);
        let copied: Vec<u32> = (0..5).map(|i| r.read(RAM + 0x800 + 4 * i)).collect();
        assert_eq!(copied, [0xA0, 0xA1, 0xB0, 0xB1, 0xB2]);
        assert_eq!(r.read(0x5000_0400) & 2, 2, "channel 1's flag from the null trigger");
        assert_eq!(r.read(CH + 0x4C) & CTRL_BUSY, 0);
    }

    #[test]
    fn an_abort_stops_a_channel_without_its_interrupt() {
        let mut r = Rig::new();
        r.write(0x5000_0420, (1 << 16) | 1000);
        r.write(CH + 8, 10);
        r.write(CH + 0xC, 1 | WORD | u32::from(TREQ_TIMER0) << 15);
        assert!(r.read(CH + 0xC) & CTRL_BUSY != 0);
        r.write(0x5000_0444, 1);
        assert_eq!(r.read(CH + 0xC) & CTRL_BUSY, 0);
        assert_eq!(r.read(0x5000_0400), 0);
        assert_eq!(r.next_event(), None);
    }
}
