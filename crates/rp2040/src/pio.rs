//! PIO0/PIO1: four state machines each, cycle-exact on their clock
//! dividers. They run lazily: up to the current cycle whenever the core
//! accesses the block and at the machine's events. Pin changes are logged
//! with the cycle of the instruction that made them.
//!
//! A state machine waiting on a stall (FIFO, IRQ, pin) sleeps until
//! something it could wait for changes; a `JMP X--`/`JMP Y--` to itself (a
//! delay loop) is fast-forwarded in one step.

use std::collections::VecDeque;

use emu_core::{Clock, DeviceCtx};

use crate::dma::SharedDma;
use crate::pins::SharedPins;
use crate::regs::{Block, Op};

/// System interrupt lines of PIO0 (IRQ 0, IRQ 1); PIO1 follows.
pub const PIO0_IRQ_0: u32 = 7;

#[derive(Clone, Debug)]
struct Sm {
    clkdiv: u32,
    execctrl: u32,
    shiftctrl: u32,
    pinctrl: u32,
    pc: u8,
    x: u32,
    y: u32,
    isr: u32,
    osr: u32,
    isr_count: u32,
    osr_count: u32,
    tx: VecDeque<u32>,
    rx: VecDeque<u32>,
    /// Time of the next clock tick of this state machine, in 1/256 cycles.
    next: u64,
    /// The last instruction stalled; it is retried once something changes.
    stalled: bool,
    /// Instruction forced by SMx_INSTR or an EXEC destination.
    exec: Option<u16>,
    /// IRQ ... WAIT has set its flag and waits for it to be cleared.
    irq_waiting: bool,
    current: u16,
}

impl Sm {
    fn new() -> Self {
        Self {
            clkdiv: 0x0001_0000,
            execctrl: 0x0001_F000,
            shiftctrl: 0x000C_0000,
            pinctrl: 0x1400_0000,
            pc: 0,
            x: 0,
            y: 0,
            isr: 0,
            osr: 0,
            isr_count: 0,
            osr_count: 32,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            next: 0,
            stalled: false,
            exec: None,
            irq_waiting: false,
            current: 0,
        }
    }

    /// Clock divider in 1/256 cycles per tick.
    fn div(&self) -> u64 {
        let int = u64::from(self.clkdiv >> 16);
        let frac = u64::from((self.clkdiv >> 8) & 0xFF);
        (if int == 0 { 65536 } else { int }) * 256 + frac
    }

    fn tx_cap(&self) -> usize {
        match self.shiftctrl >> 30 {
            0b01 => 8, // FJOIN_TX
            0b10 => 0, // FJOIN_RX
            _ => 4,
        }
    }

    fn rx_cap(&self) -> usize {
        match self.shiftctrl >> 30 {
            0b01 => 0,
            0b10 => 8,
            _ => 4,
        }
    }

    fn push_thresh(&self) -> u32 {
        match (self.shiftctrl >> 20) & 0x1F {
            0 => 32,
            n => n,
        }
    }

    fn pull_thresh(&self) -> u32 {
        match (self.shiftctrl >> 25) & 0x1F {
            0 => 32,
            n => n,
        }
    }

    fn wrap(&self) -> (u8, u8) {
        (((self.execctrl >> 7) & 0x1F) as u8, ((self.execctrl >> 12) & 0x1F) as u8)
    }

    fn advance_pc(&mut self) {
        let (bottom, top) = self.wrap();
        self.pc = if self.pc == top { bottom } else { (self.pc + 1) & 0x1F };
    }

    /// The first tick at or after `cycle` (on this machine's tick grid).
    fn align(&mut self, cycle: u64) {
        let target = cycle.saturating_mul(256);
        if self.next < target {
            let d = self.div();
            self.next += (target - self.next).div_ceil(d) * d;
        }
    }

    fn restart(&mut self) {
        self.isr_count = 0;
        self.osr_count = 32;
        self.stalled = false;
        self.exec = None;
        self.irq_waiting = false;
    }
}

enum Outcome {
    Stall,
    /// Done; `jumped`: the PC was written; `exec_next`: an instruction was
    /// forced for the next cycle (this one's delay is skipped).
    Done {
        jumped: bool,
        exec_next: bool,
    },
}

pub struct Pio {
    index: usize,
    pins: SharedPins,
    instr: [u16; 32],
    enabled: u8,
    sm: [Sm; 4],
    /// IRQ flags 0..7.
    irq: u8,
    inte: [u32; 2],
    intf: [u32; 2],
    fdebug: u32,
    input_sync_bypass: u32,
    out: u32,
    oe: u32,
    /// The state machines have run up to (excluding) this cycle.
    now: u64,
    warned: bool,
    /// Channels paced by the FIFOs feed and drain them directly.
    dma: Option<SharedDma>,
}

impl Pio {
    pub fn new(index: usize, pins: SharedPins) -> Self {
        Self {
            index,
            pins,
            instr: [0; 32],
            enabled: 0,
            sm: std::array::from_fn(|_| Sm::new()),
            irq: 0,
            inte: [0; 2],
            intf: [0; 2],
            fdebug: 0,
            input_sync_bypass: 0,
            out: 0,
            oe: 0,
            now: 0,
            warned: false,
            dma: None,
        }
    }

    /// The DMA whose channels the FIFOs pace.
    pub fn with_dma(mut self, dma: SharedDma) -> Self {
        self.dma = Some(dma);
        self
    }

    /// Fills the TX FIFO of state machine `i` from a DMA channel it paces
    /// (the DMA keeps it full).
    fn fill_tx(&mut self, ctx: &mut DeviceCtx, i: usize, cycle: u64) {
        let Some(dma) = self.dma.clone() else { return };
        while self.sm[i].tx.len() < self.sm[i].tx_cap() {
            let Some(v) = dma.borrow_mut().feed_tx(ctx, self.index, i, cycle) else { break };
            self.sm[i].tx.push_back(v);
        }
    }

    /// The next word of the TX FIFO (a pull at `cycle`).
    fn tx_pop(&mut self, ctx: &mut DeviceCtx, i: usize, cycle: u64) -> Option<u32> {
        self.fill_tx(ctx, i, cycle);
        let v = self.sm[i].tx.pop_front();
        if v.is_some() {
            self.fill_tx(ctx, i, cycle);
        }
        v
    }

    /// Hands the RX FIFO's words to a DMA channel it paces (at `cycle`).
    fn drain_rx(&mut self, ctx: &mut DeviceCtx, i: usize, cycle: u64) {
        let Some(dma) = self.dma.clone() else { return };
        while let Some(&v) = self.sm[i].rx.front() {
            if !dma.borrow_mut().take_rx(ctx, self.index, i, v, cycle) {
                break;
            }
            self.sm[i].rx.pop_front();
        }
    }

    /// The earliest end of a DMA channel paced by a FIFO: each tick of a
    /// state machine pulls or pushes one word at most.
    fn dma_end(&self) -> Option<u64> {
        let dma = self.dma.as_ref()?.try_borrow().ok()?;
        if !dma.any_busy() {
            return None;
        }
        (0..4)
            .filter(|&i| self.running(i))
            .filter_map(|i| {
                let sm = &self.sm[i];
                let tx = dma.pio_remaining(self.index, i as u8).map(|left| {
                    let room = sm.tx_cap().saturating_sub(sm.tx.len()) as u32;
                    u64::from(left.saturating_sub(room).max(1))
                });
                let rx = dma.pio_remaining(self.index, 4 + i as u8).map(|left| u64::from(left.max(1)));
                let words = [tx, rx].into_iter().flatten().min()?;
                Some((sm.next + (words - 1) * sm.div()).div_ceil(256))
            })
            .min()
    }

    fn intr(&self) -> u32 {
        let mut v = u32::from(self.irq & 0xF) << 8;
        for (i, sm) in self.sm.iter().enumerate() {
            if !sm.rx.is_empty() {
                v |= 1 << i;
            }
            if sm.tx.len() < sm.tx_cap() {
                v |= 1 << (4 + i);
            }
        }
        v
    }

    fn sync_irqs(&self, ctx: &mut DeviceCtx) {
        let intr = self.intr();
        for line in 0..2 {
            let level = (intr | self.intf[line]) & self.inte[line] != 0;
            ctx.set_irq(PIO0_IRQ_0 + 2 * self.index as u32 + line as u32, level);
        }
    }

    /// Lets stalled state machines retry from `cycle` on.
    fn wake_all(&mut self, cycle: u64) {
        for sm in &mut self.sm {
            if sm.stalled {
                sm.stalled = false;
                sm.align(cycle);
            }
        }
    }

    fn running(&self, i: usize) -> bool {
        self.enabled & (1 << i) != 0 && !self.sm[i].stalled
    }

    /// Runs the state machines up to `target` (inclusive of instructions
    /// that start before it).
    fn run_to(&mut self, ctx: &mut DeviceCtx, target: u64) {
        if target < self.now {
            return;
        }
        let limit = target.saturating_mul(256);
        while let Some(i) = (0..4).filter(|&i| self.running(i)).min_by_key(|&i| (self.sm[i].next, i))
            && self.sm[i].next < limit
        {
            self.step(ctx, i, limit);
        }
        self.now = target;
        self.sync_irqs(ctx);
    }

    fn levels(&self) -> u32 {
        let p = self.pins.borrow();
        (0..30).filter(|&n| p.level(n)).fold(0, |acc, n| acc | (1 << n))
    }

    fn set_pins(&mut self, ctx: &DeviceCtx, cycle: u64, base: u32, count: u32, data: u32, dirs: bool) {
        if count == 0 {
            return;
        }
        let mut mask = 0u32;
        for k in 0..count {
            mask |= 1 << ((base + k) % 32);
        }
        let value = data.rotate_left(base) & mask;
        let reg = if dirs { &mut self.oe } else { &mut self.out };
        let new = (*reg & !mask) | value;
        if new == *reg {
            return;
        }
        *reg = new;
        let mut p = self.pins.borrow_mut();
        p.pio_out[self.index] = self.out;
        p.pio_oe[self.index] = self.oe;
        p.refresh(ctx.clock.ps_at_cycle(cycle));
    }

    /// Executes one instruction of state machine `i` at its next tick.
    fn step(&mut self, ctx: &mut DeviceCtx, i: usize, limit: u64) {
        let cycle = self.sm[i].next / 256;
        let forced = self.sm[i].exec.take();
        let at = self.sm[i].pc;
        let instr = forced.unwrap_or(self.instr[self.sm[i].pc as usize]);
        self.sm[i].current = instr;

        // delay / side-set field
        let sm = &self.sm[i];
        let count = sm.pinctrl >> 29;
        let side_en = sm.execctrl & (1 << 30) != 0;
        let field = u32::from(instr >> 8) & 0x1F;
        let delay = field & ((1 << (5 - count)) - 1);
        let side = field >> (5 - count);
        let (apply, bits, data) = if side_en && count > 0 {
            (side >> (count - 1) & 1 != 0, count - 1, side & ((1 << (count - 1)) - 1))
        } else {
            (count > 0, count, side)
        };
        if apply {
            let base = (sm.pinctrl >> 10) & 0x1F;
            let dirs = sm.execctrl & (1 << 29) != 0;
            self.set_pins(ctx, cycle, base, bits, data, dirs);
        }

        match self.execute(ctx, i, instr, cycle) {
            Outcome::Stall => {
                let sm = &mut self.sm[i];
                if forced.is_some() {
                    sm.exec = forced;
                }
                sm.stalled = true;
                sm.next += sm.div();
            }
            Outcome::Done { jumped, exec_next } => {
                let sm = &mut self.sm[i];
                if !jumped && forced.is_none() {
                    sm.advance_pc();
                }
                let d = sm.div();
                let ticks = 1 + if exec_next { 0 } else { u64::from(delay) };
                sm.next += ticks * d;
                // A delay loop (JMP X--/Y-- to itself) only counts down its
                // register: skip the iterations that end before `limit`.
                let op = instr >> 13;
                let cond = (instr >> 5) & 7;
                let self_loop = forced.is_none() && u16::from(at) == instr & 0x1F;
                if op == 0 && (cond == 2 || cond == 4) && jumped && self_loop && sm.exec.is_none() {
                    let per = ticks * d;
                    let counter = if cond == 2 { &mut sm.x } else { &mut sm.y };
                    if sm.next < limit {
                        let n = u64::from(*counter).min((limit - sm.next) / per);
                        *counter -= n as u32;
                        sm.next += n * per;
                    }
                }
                // Other machines may wait for what this one did.
                let next = cycle + 1;
                for (j, other) in self.sm.iter_mut().enumerate() {
                    if j != i && other.stalled {
                        other.stalled = false;
                        other.align(next);
                    }
                }
            }
        }
    }

    fn execute(&mut self, ctx: &mut DeviceCtx, i: usize, instr: u16, cycle: u64) -> Outcome {
        let op = instr >> 13;
        let arg1 = u32::from(instr >> 5) & 7;
        let arg2 = u32::from(instr) & 0x1F;
        let done = Outcome::Done { jumped: false, exec_next: false };
        match op {
            // JMP
            0 => {
                let levels = if arg1 == 6 { self.levels() } else { 0 };
                let sm = &mut self.sm[i];
                let take = match arg1 {
                    0 => true,
                    1 => sm.x == 0,
                    2 => {
                        let t = sm.x != 0;
                        sm.x = sm.x.wrapping_sub(1);
                        t
                    }
                    3 => sm.y == 0,
                    4 => {
                        let t = sm.y != 0;
                        sm.y = sm.y.wrapping_sub(1);
                        t
                    }
                    5 => sm.x != sm.y,
                    6 => levels & (1 << ((sm.execctrl >> 24) & 0x1F)) != 0,
                    _ => sm.osr_count < sm.pull_thresh(),
                };
                if take {
                    sm.pc = arg2 as u8;
                }
                Outcome::Done { jumped: take, exec_next: false }
            }
            // WAIT
            1 => {
                let pol = instr & 0x80 != 0;
                let src = (instr >> 5) & 3;
                let met = match src {
                    0 => (self.levels() >> arg2) & 1 != 0,
                    1 => {
                        let base = (self.sm[i].pinctrl >> 15) & 0x1F;
                        (self.levels() >> ((base + arg2) % 32)) & 1 != 0
                    }
                    2 => {
                        let flag = irq_index(arg2, i);
                        let set = self.irq & (1 << flag) != 0;
                        if set == pol && pol {
                            self.irq &= !(1 << flag);
                        }
                        set
                    }
                    _ => pol,
                } == pol;
                if met { done } else { Outcome::Stall }
            }
            // IN
            2 => {
                let n = if arg2 == 0 { 32 } else { arg2 };
                self.drain_rx(ctx, i, cycle);
                let sm = &self.sm[i];
                let autopush = sm.shiftctrl & (1 << 16) != 0;
                if autopush && sm.isr_count + n >= sm.push_thresh() && sm.rx.len() >= sm.rx_cap() {
                    self.fdebug |= 1 << i; // RXSTALL
                    return Outcome::Stall;
                }
                let data = match arg1 {
                    0 => {
                        let base = (sm.pinctrl >> 15) & 0x1F;
                        self.levels().rotate_right(base)
                    }
                    1 => sm.x,
                    2 => sm.y,
                    6 => sm.isr,
                    7 => sm.osr,
                    _ => 0,
                };
                let sm = &mut self.sm[i];
                let mask = if n == 32 { u32::MAX } else { (1 << n) - 1 };
                let data = data & mask;
                sm.isr = if sm.shiftctrl & (1 << 18) != 0 {
                    if n == 32 { data } else { (sm.isr >> n) | (data << (32 - n)) }
                } else if n == 32 {
                    data
                } else {
                    (sm.isr << n) | data
                };
                sm.isr_count = (sm.isr_count + n).min(32);
                if autopush && sm.isr_count >= sm.push_thresh() {
                    sm.rx.push_back(sm.isr);
                    sm.isr = 0;
                    sm.isr_count = 0;
                    self.drain_rx(ctx, i, cycle);
                }
                done
            }
            // OUT
            3 => {
                let n = if arg2 == 0 { 32 } else { arg2 };
                let autopull = self.sm[i].shiftctrl & (1 << 17) != 0;
                if autopull && self.sm[i].osr_count >= self.sm[i].pull_thresh() {
                    match self.tx_pop(ctx, i, cycle) {
                        Some(v) => {
                            self.sm[i].osr = v;
                            self.sm[i].osr_count = 0;
                        }
                        None => {
                            self.fdebug |= 1 << (24 + i); // TXSTALL
                            return Outcome::Stall;
                        }
                    }
                }
                let sm = &mut self.sm[i];
                let right = sm.shiftctrl & (1 << 19) != 0;
                let data = if n == 32 {
                    let d = sm.osr;
                    sm.osr = 0;
                    d
                } else if right {
                    let d = sm.osr & ((1 << n) - 1);
                    sm.osr >>= n;
                    d
                } else {
                    let d = sm.osr >> (32 - n);
                    sm.osr <<= n;
                    d
                };
                sm.osr_count = (sm.osr_count + n).min(32);
                let pinctrl = sm.pinctrl;
                match arg1 {
                    0 => self.set_pins(ctx, cycle, pinctrl & 0x1F, (pinctrl >> 20) & 0x3F, data, false),
                    1 => self.sm[i].x = data,
                    2 => self.sm[i].y = data,
                    4 => self.set_pins(ctx, cycle, pinctrl & 0x1F, (pinctrl >> 20) & 0x3F, data, true),
                    5 => {
                        self.sm[i].pc = (data & 0x1F) as u8;
                        return Outcome::Done { jumped: true, exec_next: false };
                    }
                    6 => {
                        self.sm[i].isr = data;
                        self.sm[i].isr_count = n;
                    }
                    7 => {
                        self.sm[i].exec = Some(data as u16);
                        return Outcome::Done { jumped: false, exec_next: true };
                    }
                    _ => {}
                }
                done
            }
            // PUSH / PULL
            4 => {
                let if_flag = instr & 0x40 != 0;
                let block = instr & 0x20 != 0;
                if instr & 0x80 == 0 {
                    self.drain_rx(ctx, i, cycle);
                    let sm = &mut self.sm[i];
                    if if_flag && sm.isr_count < sm.push_thresh() {
                        return done;
                    }
                    if sm.rx.len() >= sm.rx_cap() {
                        if block {
                            self.fdebug |= 1 << i; // RXSTALL
                            return Outcome::Stall;
                        }
                    } else {
                        sm.rx.push_back(sm.isr);
                    }
                    sm.isr = 0;
                    sm.isr_count = 0;
                    self.drain_rx(ctx, i, cycle);
                } else {
                    let sm = &self.sm[i];
                    if if_flag && sm.osr_count < sm.pull_thresh() {
                        return done;
                    }
                    let v = self.tx_pop(ctx, i, cycle);
                    let sm = &mut self.sm[i];
                    match v {
                        Some(v) => sm.osr = v,
                        None if block => {
                            self.fdebug |= 1 << (24 + i); // TXSTALL
                            return Outcome::Stall;
                        }
                        None => sm.osr = sm.x,
                    }
                    sm.osr_count = 0;
                }
                done
            }
            // MOV
            5 => {
                let sm = &self.sm[i];
                let src = u32::from(instr) & 7;
                let value = match src {
                    0 => {
                        let base = (sm.pinctrl >> 15) & 0x1F;
                        self.levels().rotate_right(base)
                    }
                    1 => sm.x,
                    2 => sm.y,
                    5 => {
                        let n = (sm.execctrl & 0xF) as usize;
                        let low = if sm.execctrl & (1 << 4) != 0 { sm.rx.len() < n } else { sm.tx.len() < n };
                        if low { u32::MAX } else { 0 }
                    }
                    6 => sm.isr,
                    7 => sm.osr,
                    _ => 0,
                };
                let value = match (instr >> 3) & 3 {
                    1 => !value,
                    2 => value.reverse_bits(),
                    _ => value,
                };
                let pinctrl = sm.pinctrl;
                let sm = &mut self.sm[i];
                match arg1 {
                    0 => self.set_pins(ctx, cycle, pinctrl & 0x1F, (pinctrl >> 20) & 0x3F, value, false),
                    1 => sm.x = value,
                    2 => sm.y = value,
                    4 => {
                        sm.exec = Some(value as u16);
                        return Outcome::Done { jumped: false, exec_next: true };
                    }
                    5 => {
                        sm.pc = (value & 0x1F) as u8;
                        return Outcome::Done { jumped: true, exec_next: false };
                    }
                    6 => {
                        sm.isr = value;
                        sm.isr_count = 0;
                    }
                    7 => {
                        sm.osr = value;
                        sm.osr_count = 0;
                    }
                    _ => {}
                }
                done
            }
            // IRQ
            6 => {
                let flag = irq_index(arg2, i);
                let clear = instr & 0x40 != 0;
                let wait = instr & 0x20 != 0;
                if clear {
                    self.irq &= !(1 << flag);
                    return done;
                }
                let sm = &mut self.sm[i];
                if sm.irq_waiting {
                    if self.irq & (1 << flag) != 0 {
                        return Outcome::Stall;
                    }
                    sm.irq_waiting = false;
                    return done;
                }
                self.irq |= 1 << flag;
                if wait {
                    sm.irq_waiting = true;
                    return Outcome::Stall;
                }
                done
            }
            // SET
            _ => {
                let pinctrl = self.sm[i].pinctrl;
                match arg1 {
                    0 => self.set_pins(ctx, cycle, (pinctrl >> 5) & 0x1F, (pinctrl >> 26) & 7, arg2, false),
                    1 => self.sm[i].x = arg2,
                    2 => self.sm[i].y = arg2,
                    4 => self.set_pins(ctx, cycle, (pinctrl >> 5) & 0x1F, (pinctrl >> 26) & 7, arg2, true),
                    _ => {}
                }
                done
            }
        }
    }

    /// Earliest instruction of a state machine that is not waiting.
    fn next_instruction(&self) -> Option<u64> {
        (0..4).filter(|&i| self.running(i)).map(|i| self.sm[i].next.div_ceil(256)).min()
    }
}

/// IRQ index of an IRQ/WAIT instruction: bit 4 adds the state machine
/// number to the two low bits.
fn irq_index(arg: u32, sm: usize) -> u32 {
    if arg & 0x10 != 0 { (arg & 4) | ((arg + sm as u32) & 3) } else { arg & 7 }
}

impl Block for Pio {
    fn name(&self) -> &str {
        if self.index == 0 { "PIO0" } else { "PIO1" }
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32 {
        self.run_to(ctx, ctx.now());
        let v =
            match off {
                0x000 => u32::from(self.enabled),
                0x004 => {
                    let mut v = 0;
                    for (i, sm) in self.sm.iter().enumerate() {
                        if sm.rx.len() >= sm.rx_cap() {
                            v |= 1 << i;
                        }
                        if sm.rx.is_empty() {
                            v |= 1 << (8 + i);
                        }
                        if sm.tx.len() >= sm.tx_cap() {
                            v |= 1 << (16 + i);
                        }
                        if sm.tx.is_empty() {
                            v |= 1 << (24 + i);
                        }
                    }
                    v
                }
                0x008 => self.fdebug,
                0x00C => self.sm.iter().enumerate().fold(0, |acc, (i, sm)| {
                    acc | (sm.tx.len() as u32) << (8 * i) | (sm.rx.len() as u32) << (8 * i + 4)
                }),
                0x020..=0x02C => {
                    let i = ((off - 0x20) / 4) as usize;
                    match self.sm[i].rx.pop_front() {
                        Some(v) => {
                            self.wake_all(ctx.now());
                            v
                        }
                        None => {
                            self.fdebug |= 1 << (8 + i); // RXUNDER
                            0
                        }
                    }
                }
                0x030 => u32::from(self.irq),
                0x038 => self.input_sync_bypass,
                0x03C => self.out,
                0x040 => self.oe,
                0x044 => 0x0020_0404, // 32 instructions, 4 state machines, FIFO depth 4
                0x0C8..=0x124 => {
                    let i = ((off - 0xC8) / 0x18) as usize;
                    let sm = &self.sm[i];
                    match (off - 0xC8) % 0x18 {
                        0x00 => sm.clkdiv,
                        0x04 => sm.execctrl | if sm.stalled && sm.exec.is_some() { 1 << 31 } else { 0 },
                        0x08 => sm.shiftctrl,
                        0x0C => u32::from(sm.pc),
                        0x10 => u32::from(sm.current),
                        _ => sm.pinctrl,
                    }
                }
                0x128 => self.intr(),
                0x12C => self.inte[0],
                0x130 => self.intf[0],
                0x134 => (self.intr() | self.intf[0]) & self.inte[0],
                0x138 => self.inte[1],
                0x13C => self.intf[1],
                0x140 => (self.intr() | self.intf[1]) & self.inte[1],
                _ => 0,
            };
        self.sync_irqs(ctx);
        v
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        let now = ctx.now();
        self.run_to(ctx, now);
        match off {
            0x000 => {
                let v = op.apply(u32::from(self.enabled), value);
                let newly = v as u8 & 0xF & !self.enabled;
                for i in 0..4 {
                    let sm = &mut self.sm[i];
                    if v & (1 << (4 + i)) != 0 {
                        sm.restart();
                    }
                    if v & (1 << (8 + i)) != 0 || newly & (1 << i) != 0 {
                        // CLKDIV_RESTART / enable: the first tick is the next cycle
                        sm.next = (now + 1) * 256;
                    }
                }
                self.enabled = v as u8 & 0xF;
                if newly != 0 && log::log_enabled!(log::Level::Debug) {
                    let prog: Vec<String> = self.instr.iter().map(|i| format!("{i:04x}")).collect();
                    log::debug!("Pico {}: enabled SMs {newly:#x}, program {}", self.name(), prog.join(" "));
                    for (i, sm) in self.sm.iter().enumerate().filter(|(i, _)| newly & (1 << i) != 0) {
                        log::debug!(
                            "  SM{i}: clkdiv {:#010x} execctrl {:#010x} shiftctrl {:#010x} pinctrl {:#010x} pc {}",
                            sm.clkdiv,
                            sm.execctrl,
                            sm.shiftctrl,
                            sm.pinctrl,
                            sm.pc
                        );
                    }
                }
            }
            0x008 => self.fdebug &= !op.clears(value),
            0x010..=0x01C => {
                let i = ((off - 0x10) / 4) as usize;
                let sm = &mut self.sm[i];
                if sm.tx.len() < sm.tx_cap() {
                    sm.tx.push_back(value);
                } else {
                    self.fdebug |= 1 << (16 + i); // TXOVER
                }
                self.wake_all(now);
            }
            0x030 => {
                self.irq &= !(op.clears(value) as u8);
                self.wake_all(now);
            }
            0x034 => {
                self.irq |= op.apply(0, value) as u8;
                self.wake_all(now);
            }
            0x038 => self.input_sync_bypass = op.apply(self.input_sync_bypass, value),
            0x048..=0x0C4 => {
                let n = ((off - 0x48) / 4) as usize;
                self.instr[n] = op.apply(u32::from(self.instr[n]), value) as u16;
            }
            0x0C8..=0x124 => {
                let i = ((off - 0xC8) / 0x18) as usize;
                let sm = &mut self.sm[i];
                match (off - 0xC8) % 0x18 {
                    0x00 => {
                        sm.clkdiv = op.apply(sm.clkdiv, value) & 0xFFFF_FF00;
                        sm.next = sm.next.max((now + 1) * 256);
                    }
                    0x04 => {
                        sm.execctrl = op.apply(sm.execctrl, value) & 0x7FFF_FF9F;
                        if sm.execctrl & (0x3 << 17) != 0 && !self.warned {
                            self.warned = true;
                            log::warn!("Pico: PIO OUT_STICKY/INLINE_OUT_EN are not modelled");
                        }
                    }
                    0x08 => {
                        let old = sm.shiftctrl;
                        sm.shiftctrl = op.apply(sm.shiftctrl, value) & 0xFFFF_0000;
                        if (old ^ sm.shiftctrl) & (3 << 30) != 0 {
                            // changing the FIFO join clears both FIFOs
                            sm.tx.clear();
                            sm.rx.clear();
                        }
                    }
                    0x10 => {
                        // SMx_INSTR: executed at once, even when disabled
                        sm.exec = Some(value as u16);
                        sm.stalled = false;
                        sm.next = now * 256;
                        let limit = (now + 1) * 256;
                        self.step(ctx, i, limit);
                        if self.enabled & (1 << i) != 0 || self.sm[i].stalled {
                            self.sm[i].align(now + 1);
                        }
                    }
                    0x14 => sm.pinctrl = op.apply(sm.pinctrl, value),
                    _ => {}
                }
            }
            0x12C => self.inte[0] = op.apply(self.inte[0], value) & 0xFFF,
            0x130 => self.intf[0] = op.apply(self.intf[0], value) & 0xFFF,
            0x138 => self.inte[1] = op.apply(self.inte[1], value) & 0xFFF,
            0x13C => self.intf[1] = op.apply(self.intf[1], value) & 0xFFF,
            _ => {}
        }
        self.sync_irqs(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.run_to(ctx, ctx.now());
    }

    fn next_event(&self) -> Option<u64> {
        // Only interrupts need the state machines on time (also the DMA's,
        // when a FIFO paces a channel); pins and FIFOs are brought up to
        // date when they are looked at.
        let irq = if self.inte[0] | self.inte[1] != 0 { self.next_instruction() } else { None };
        [irq, self.dma_end()].into_iter().flatten().min()
    }

    fn read_stable_until(&self, off: u32, _clock: &Clock) -> u64 {
        match off {
            0x020..=0x02C => 0,
            _ => self.next_instruction().unwrap_or(u64::MAX),
        }
    }

    fn reset(&mut self) {
        let (pins, dma) = (self.pins.clone(), self.dma.take());
        *self = Self::new(self.index, pins);
        self.dma = dma;
    }
}

impl Pio {
    /// Brings the state machines up to `ctx.now()` (pin changes are logged).
    pub fn catch_up(&mut self, ctx: &mut DeviceCtx) {
        self.run_to(ctx, ctx.now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pins::{FUNC_PIO0, PinEdge};
    use emu_core::nvic::Nvic;

    struct Rig {
        pio: Pio,
        pins: SharedPins,
        clock: Clock,
        nvic: Nvic,
    }

    impl Rig {
        fn new(program: &[u16]) -> Self {
            let pins = SharedPins::default();
            let mut pio = Pio::new(0, pins.clone());
            pio.instr[..program.len()].copy_from_slice(program);
            for n in 0..30 {
                pins.borrow_mut().ctrl[n] = FUNC_PIO0;
            }
            Self { pio, pins, clock: Clock::new(125_000_000), nvic: Nvic::new() }
        }

        fn write(&mut self, off: u32, v: u32) {
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.pio.write(&mut ctx, off, Op::Write, v);
        }

        fn run(&mut self, cycles: u64) {
            self.clock.advance(cycles);
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.pio.update(&mut ctx);
        }

        /// Edge times in cycles of pin `pin`.
        fn edges(&mut self, pin: u8) -> Vec<(u64, Option<bool>)> {
            let clock = self.clock.clone();
            self.pins
                .borrow_mut()
                .take_edges()
                .into_iter()
                .filter(|e: &PinEdge| e.pin == pin)
                .map(|e| (clock.cycle_at_ps(e.ps), e.level))
                .collect()
        }
    }

    // pioasm: set pindirs, 1 / loop: set pins, 1 [1] / set pins, 0 / jmp loop
    const SQUARE: [u16; 4] = [0xE081, 0xE101, 0xE000, 0x0001];

    #[test]
    fn set_with_delay_and_wrap() {
        let mut r = Rig::new(&SQUARE);
        r.write(0xC8 + 0x14, (1 << 26) | (2 << 5)); // SET_COUNT 1, SET_BASE 2
        r.write(0x000, 1);
        r.run(20);
        // enabled at cycle 0: instruction 0 at 1, then the loop takes 4 cycles
        // (2 for the high phase, 1 low, 1 jmp)
        let e = r.edges(2);
        assert_eq!(e[0], (1, Some(false)), "pindirs: driven low");
        let highs: Vec<u64> = e.iter().filter(|(_, l)| *l == Some(true)).map(|(t, _)| *t).collect();
        assert_eq!(&highs[..3], &[2, 6, 10]);
        let lows: Vec<u64> = e.iter().skip(1).filter(|(_, l)| *l == Some(false)).map(|(t, _)| *t).collect();
        assert_eq!(&lows[..3], &[4, 8, 12]);
    }

    #[test]
    fn delay_loop_is_fast_forwarded_exactly() {
        // pull block / mov x, osr / set pins, 1 / loop: jmp x-- loop / set pins, 0
        let prog = [0x80A0, 0xA027, 0xE001, 0x0043, 0xE000];
        let mut r = Rig::new(&prog);
        r.write(0xC8 + 0x04, 4 << 12); // wrap top 4
        r.write(0xC8 + 0x14, (1 << 26) | (5 << 5));
        r.write(0xC8 + 0x10, 0xE081); // forced: set pindirs, 1
        r.write(0x010, 1_000_000);
        r.write(0x000, 1);
        r.run(2_000_100);
        let e = r.edges(5);
        // set at 3, the loop runs 1_000_001 times (one per cycle), set 0 after
        let high = e.iter().find(|(_, l)| *l == Some(true)).unwrap().0;
        let low = e.iter().rev().find(|(_, l)| *l == Some(false)).unwrap().0;
        assert_eq!(high, 3);
        assert_eq!(low - high, 1_000_001 + 1);
    }

    #[test]
    fn counted_loop_runs_once_per_count() {
        // pull block / mov x, osr / loop: set pins, 1 [3] / set pins, 0 [3] / jmp x-- loop / end: jmp end
        let prog = [0x80A0, 0xA027, 0xE301, 0xE300, 0x0042, 0x0005];
        let mut r = Rig::new(&prog);
        r.write(0xC8 + 0x14, (1 << 26) | (5 << 5));
        r.write(0xC8 + 0x10, 0xE081); // forced: set pindirs, 1
        r.write(0x010, 9);
        r.write(0x000, 1);
        r.run(1_000);
        let highs = r.edges(5).iter().filter(|(_, l)| *l == Some(true)).count();
        assert_eq!(highs, 10, "x + 1 iterations");
    }

    #[test]
    fn fractional_divider_keeps_the_average_rate() {
        let mut r = Rig::new(&SQUARE);
        r.write(0xC8 + 0x14, (1 << 26) | (2 << 5));
        r.write(0xC8, (2 << 16) | (128 << 8)); // divide by 2.5
        r.write(0x000, 1);
        r.run(10_001);
        let highs: Vec<u64> = r.edges(2).iter().filter(|(_, l)| *l == Some(true)).map(|(t, _)| *t).collect();
        // one period = 4 ticks = 10 cycles
        assert_eq!(highs.len(), 1000);
        assert_eq!(highs[999] - highs[0], 9990);
    }
}
