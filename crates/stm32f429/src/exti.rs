//! EXTI (external interrupt/event controller) and the EXTI port selection of
//! SYSCFG.
//!
//! Behaviour measured on the board (HwCheck `exti`): an edge sets the
//! pending bit only while the line is unmasked in IMR; a SWIER bit set while
//! masked stays set without making the line pending; clearing PR also clears
//! SWIER; SYSCFG ignores writes and reads 0 while its clock is off.
//! Events (EMR) are stored but do not wake the core.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::gpio::{GpioBank, NUM_PORTS, SharedGpio};
use crate::rcc::SharedClocks;

const LINES: u32 = 0x7F_FFFF; // 23 lines
const SYSCFGEN: u32 = 1 << 14; // RCC_APB2ENR

/// NVIC interrupts and the EXTI lines they serve (lines 16..22: PVD, RTC
/// alarm, USB OTG FS wakeup, Ethernet wakeup, USB OTG HS wakeup,
/// tamper/time stamp, RTC wakeup).
const IRQ_LINES: [(u32, u32); 14] = [
    (6, 1 << 0),
    (7, 1 << 1),
    (8, 1 << 2),
    (9, 1 << 3),
    (10, 1 << 4),
    (23, 0x3E0),
    (40, 0xFC00),
    (1, 1 << 16),
    (41, 1 << 17),
    (42, 1 << 18),
    (62, 1 << 19),
    (76, 1 << 20),
    (2, 1 << 21),
    (3, 1 << 22),
];

#[derive(Default)]
pub struct ExtiState {
    exticr: [u32; 4],
    imr: u32,
    emr: u32,
    rtsr: u32,
    ftsr: u32,
    swier: u32,
    pr: u32,
}

impl ExtiState {
    fn port_of(&self, line: usize) -> usize {
        ((self.exticr[line / 4] >> (4 * (line % 4))) & 0xF) as usize
    }

    /// Tells the GPIO bank which pins to report edges of.
    fn update_watch(&self, gpio: &mut GpioBank) {
        let mut watch = [0u16; NUM_PORTS];
        let active = (self.rtsr | self.ftsr) & self.imr;
        for line in 0..16 {
            let port = self.port_of(line);
            if active & (1 << line) != 0 && port < NUM_PORTS {
                watch[port] |= 1 << line;
            }
        }
        for (p, w) in gpio.ports.iter_mut().zip(watch) {
            p.exti_watch = w;
            p.exti_rise &= w;
            p.exti_fall &= w;
        }
    }

    /// Turns the edges that have reached EXTI by cycle `now` into pending bits.
    fn take_edges(&mut self, gpio: &mut GpioBank, now: u64) {
        if gpio.exti_due().is_none_or(|due| due > now) {
            return;
        }
        for line in 0..16 {
            let port = self.port_of(line);
            let Some(p) = gpio.ports.get(port).filter(|p| p.exti_due <= now) else { continue };
            let bit = 1u32 << line;
            let rise = u32::from(p.exti_rise) & bit != 0 && self.rtsr & bit != 0;
            let fall = u32::from(p.exti_fall) & bit != 0 && self.ftsr & bit != 0;
            if (rise || fall) && self.imr & bit != 0 {
                self.pr |= bit;
            }
        }
        for p in gpio.ports.iter_mut().filter(|p| p.exti_due <= now) {
            p.exti_rise = 0;
            p.exti_fall = 0;
        }
    }

    fn sync_irqs(&self, ctx: &mut DeviceCtx) {
        let active = self.pr & self.imr;
        for (irq, lines) in IRQ_LINES {
            ctx.set_irq(irq, active & lines != 0);
        }
    }
}

pub type SharedExti = Rc<RefCell<ExtiState>>;

pub struct Exti {
    state: SharedExti,
    gpio: SharedGpio,
}

impl Exti {
    pub fn new(state: SharedExti, gpio: SharedGpio) -> Self {
        Self { state, gpio }
    }

    fn sync(&self, ctx: &mut DeviceCtx) {
        let mut st = self.state.borrow_mut();
        st.take_edges(&mut self.gpio.borrow_mut(), ctx.now());
        st.sync_irqs(ctx);
    }
}

impl Peripheral for Exti {
    fn name(&self) -> &str {
        "EXTI"
    }

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        self.gpio.borrow().exti_due().unwrap_or(u64::MAX)
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        self.sync(ctx);
        let st = self.state.borrow();
        let reg = match off & !3 {
            0x00 => st.imr,
            0x04 => st.emr,
            0x08 => st.rtsr,
            0x0C => st.ftsr,
            0x10 => st.swier,
            0x14 => st.pr,
            _ => 0,
        };
        extract_read(reg, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        self.sync(ctx);
        let mut st = self.state.borrow_mut();
        let st = &mut *st;
        let reg = off & !3;
        let old = match reg {
            0x00 => st.imr,
            0x04 => st.emr,
            0x08 => st.rtsr,
            0x0C => st.ftsr,
            0x10 => st.swier,
            _ => 0,
        };
        let v = merge_write(old, off, size, value) & LINES;
        match reg {
            0x00 => st.imr = v,
            0x04 => st.emr = v,
            0x08 => st.rtsr = v,
            0x0C => st.ftsr = v,
            0x10 => {
                st.pr |= v & !st.swier & st.imr;
                st.swier = v;
            }
            0x14 => {
                let clear = merge_write(0, off, size, value);
                st.pr &= !clear;
                st.swier &= !clear;
            }
            _ => return,
        }
        st.update_watch(&mut self.gpio.borrow_mut());
        st.sync_irqs(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.gpio.borrow().exti_due().is_some_and(|due| due <= ctx.now()) {
            self.sync(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.gpio.borrow().exti_due()
    }

    fn reset(&mut self) {
        let mut st = self.state.borrow_mut();
        *st = ExtiState::default();
        st.update_watch(&mut self.gpio.borrow_mut());
    }
}

/// SYSCFG: memory remap, Ethernet PHY interface, EXTI port selection,
/// compensation cell (always ready).
pub struct Syscfg {
    exti: SharedExti,
    gpio: SharedGpio,
    clocks: SharedClocks,
    memrmp: u32,
    pmc: u32,
    cmpcr: u32,
}

impl Syscfg {
    pub fn new(exti: SharedExti, gpio: SharedGpio, clocks: SharedClocks) -> Self {
        Self { exti, gpio, clocks, memrmp: 0, pmc: 0, cmpcr: 0 }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().apb2enr & SYSCFGEN != 0
    }
}

impl Peripheral for Syscfg {
    fn name(&self) -> &str {
        "SYSCFG"
    }

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let reg = match off & !3 {
            0x00 => self.memrmp,
            0x04 => self.pmc,
            o @ 0x08..=0x14 => self.exti.borrow().exticr[(o as usize - 8) / 4],
            // READY follows CMP_PD at once
            0x20 => self.cmpcr | ((self.cmpcr & 1) << 8),
            _ => 0,
        };
        extract_read(reg, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        match off & !3 {
            0x00 => self.memrmp = merge_write(self.memrmp, off, size, value) & 0x0F07,
            0x04 => self.pmc = merge_write(self.pmc, off, size, value) & 0x0080_0000,
            o @ 0x08..=0x14 => {
                let mut st = self.exti.borrow_mut();
                let mut gpio = self.gpio.borrow_mut();
                st.take_edges(&mut gpio, ctx.now());
                let i = (o as usize - 8) / 4;
                st.exticr[i] = merge_write(st.exticr[i], off, size, value) & 0xFFFF;
                st.update_watch(&mut gpio);
                st.sync_irqs(ctx);
            }
            0x20 => self.cmpcr = merge_write(self.cmpcr, off, size, value) & 1,
            _ => {}
        }
    }

    fn reset(&mut self) {
        (self.memrmp, self.pmc, self.cmpcr) = (0, 0, 0);
    }
}
