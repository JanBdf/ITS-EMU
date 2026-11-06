//! Peripherals both cores reach: one bus behind a proxy in each core's bus.
//!
//! The cores run in turns of a few microseconds, so one of them is behind
//! the other. The shared peripherals keep the time of the core ahead (their
//! clock never goes back): an access of the core behind takes effect up to a
//! turn late. Interrupt lines go to both cores' NVICs, as on the chip.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::nvic::Nvic;
use emu_core::{Bus, Clock, DeviceCtx, Peripheral, ResetKind};

use crate::dma::{self, SharedDma};
use crate::sio::{SIO_IRQ_PROC0, SIO_IRQ_PROC1};

/// The external interrupt lines of the RP2040's peripherals (0..26), except
/// the SIOs' (one per core).
const SHARED_IRQS: u32 = ((1 << 26) - 1) & !(1 << SIO_IRQ_PROC0) & !(1 << SIO_IRQ_PROC1);

struct Shared {
    bus: Bus,
    clock: Clock,
    /// Interrupt line levels the peripherals drive.
    nvic: Nvic,
    reset_hz: u32,
    /// The bus's next event, recomputed on writes and updates; reads (of
    /// polling loops, mostly) only bring the read peripheral's event in.
    /// Too early is harmless: the core updates the devices then.
    next_event: Option<u64>,
    /// Interrupt lines each core's NVIC was given.
    applied: [u32; 2],
    /// The DMA, whose transfers are made around every access and update.
    dma: Option<SharedDma>,
}

#[derive(Clone)]
pub struct SharedBus(Rc<RefCell<Shared>>);

impl SharedBus {
    pub fn new(bus: Bus, reset_hz: u32) -> Self {
        Self(Rc::new(RefCell::new(Shared {
            bus,
            clock: Clock::new(reset_hz),
            nvic: Nvic::new(),
            reset_hz,
            next_event: None,
            applied: [0; 2],
            dma: None,
        })))
    }

    /// The DMA that moves data on this bus.
    pub fn set_dma(&self, dma: SharedDma) {
        self.0.borrow_mut().dma = Some(dma);
    }

    /// Runs `f` on the bus at the time of core `core` (or later), then hands
    /// frequency changes and interrupt lines to that core. `changes`: `f`
    /// may change when peripherals have their next events (not just reads).
    fn with<R>(
        &self,
        core: usize,
        ctx: &mut DeviceCtx,
        changes: bool,
        f: impl FnOnce(&mut Bus, &mut DeviceCtx) -> R,
    ) -> R {
        let mut guard = self.0.borrow_mut();
        let s = &mut *guard;
        let now = ctx.clock.cycles();
        if now > s.clock.cycles() {
            s.clock.advance(now - s.clock.cycles());
        }
        let mut shared_ctx = DeviceCtx { clock: &mut s.clock, nvic: &mut s.nvic };
        // the DMA up to now before the access, and what the access lets it do
        let pump = |bus: &mut Bus, ctx: &mut DeviceCtx| s.dma.as_ref().is_some_and(|d| dma::pump(d, bus, ctx));
        let mut moved = pump(&mut s.bus, &mut shared_ctx);
        let result = f(&mut s.bus, &mut shared_ctx);
        moved |= pump(&mut s.bus, &mut shared_ctx);
        let accessed = s.bus.take_access_event();
        if changes || moved {
            s.next_event = s.bus.next_event();
        } else if let Some(t) = accessed {
            s.next_event = Some(s.next_event.map_or(t, |n| n.min(t)));
        }
        ctx.clock.follow(&s.clock);
        let levels = s.nvic.irq_levels_low() & SHARED_IRQS;
        // lines that changed, and those that are high: software may have
        // cleared their pending state, which a high line sets again
        let mut changed = (levels ^ s.applied[core]) | levels;
        while changed != 0 {
            let irq = changed.trailing_zeros();
            ctx.nvic.set_irq_level(irq, levels & (1 << irq) != 0);
            changed &= changed - 1;
        }
        s.applied[core] = levels;
        if let Some(kind) = s.nvic.reset_request.take() {
            ctx.nvic.reset_request = Some(kind);
        }
        result
    }

    /// Core `core`'s proxy for the peripheral window at `base` (absolute
    /// addresses).
    pub fn window(&self, core: usize, base: u32) -> Window {
        Window { shared: self.clone(), core, base }
    }

    /// Core `core`'s proxy that keeps the peripherals' time; core 0's also
    /// resets them.
    pub fn devices(&self, core: usize) -> Devices {
        Devices { shared: self.clone(), core }
    }
}

pub struct Window {
    shared: SharedBus,
    core: usize,
    base: u32,
}

impl Peripheral for Window {
    fn name(&self) -> &str {
        "shared peripherals"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        let addr = self.base + off;
        self.shared.with(self.core, ctx, false, |bus, ctx| bus.read(ctx, addr, size))
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        let addr = self.base + off;
        self.shared.with(self.core, ctx, true, |bus, ctx| bus.write(ctx, addr, size, value));
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        self.shared.0.borrow_mut().bus.read_stable_until(self.base + off, clock)
    }

    fn next_event(&self) -> Option<u64> {
        self.shared.0.borrow().next_event
    }

    fn write_is_inert(&self, off: u32) -> bool {
        self.shared.0.borrow_mut().bus.write_is_inert(self.base + off)
    }
}

pub struct Devices {
    shared: SharedBus,
    core: usize,
}

impl Peripheral for Devices {
    fn name(&self) -> &str {
        "shared peripherals"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, _off: u32, _size: u32) -> u32 {
        0
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, _off: u32, _size: u32, _value: u32) {}

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.shared.with(self.core, ctx, true, |bus, ctx| bus.update(ctx));
    }

    fn next_event(&self) -> Option<u64> {
        self.shared.0.borrow().next_event
    }

    fn system_reset(&mut self, kind: ResetKind) {
        let mut s = self.shared.0.borrow_mut();
        // the core's NVIC is reset
        s.applied[self.core] = 0;
        if self.core == 0 {
            s.bus.reset(kind);
            s.nvic = Nvic::new();
            let hz = s.reset_hz;
            s.clock.set_cpu_hz(hz);
            s.next_event = s.bus.next_event();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Holds interrupt line 5 high.
    struct Asserting;

    impl Peripheral for Asserting {
        fn name(&self) -> &str {
            "asserting"
        }
        fn read(&mut self, ctx: &mut DeviceCtx, _off: u32, _size: u32) -> u32 {
            ctx.set_irq(5, true);
            0
        }
        fn write(&mut self, _ctx: &mut DeviceCtx, _off: u32, _size: u32, _value: u32) {}
    }

    #[test]
    fn a_line_still_high_pends_again_after_software_cleared_it() {
        let mut bus = Bus::new();
        bus.attach(0x4000_0000, 0x1000, Box::new(Asserting));
        let shared = SharedBus::new(bus, 1_000_000);
        let mut window = shared.window(0, 0x4000_0000);
        let mut clock = Clock::new(1_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        window.read(&mut ctx, 0, 4);
        let exc = 16 + 5;
        assert!(ctx.nvic.is_pending(exc));
        // the SDK's irq_set_enabled clears the pending state (NVIC_ICPR) first
        ctx.nvic.clear_pending(exc);
        window.read(&mut ctx, 0, 4);
        assert!(ctx.nvic.is_pending(exc), "the line is still high");
    }
}
