//! External devices on the header pins (rotary encoder, ...). They drive and
//! watch pins through the GPIO bank and take part in the machine's time
//! keeping as a device without registers.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::ResetKind;
use emu_core::{DeviceCtx, Peripheral};
use stm32f429::gpio::{SharedGpio, port};

/// A device on the pins: like a peripheral, but without registers.
pub trait PinDevice {
    /// Brings the device up to `ctx.now()`.
    fn update(&mut self, ctx: &mut DeviceCtx);
    /// Cycle of the device's next own action.
    fn next_event(&self) -> Option<u64>;
    /// The MCU was reset (the device itself keeps running).
    fn mcu_reset(&mut self) {}
}

pub type SharedDevices = Rc<RefCell<Vec<Rc<RefCell<dyn PinDevice>>>>>;

/// The devices as one [`Peripheral`] for [`emu_core::Bus::attach_device`].
pub struct DeviceHub(pub SharedDevices);

impl Peripheral for DeviceHub {
    fn name(&self) -> &str {
        "external devices"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, _offset: u32, _size: u32) -> u32 {
        0
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, _offset: u32, _size: u32, _value: u32) {}

    fn update(&mut self, ctx: &mut DeviceCtx) {
        for d in self.0.borrow().iter() {
            d.borrow_mut().update(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.0.borrow().iter().filter_map(|d| d.borrow().next_event()).min()
    }

    fn system_reset(&mut self, _kind: ResetKind) {
        for d in self.0.borrow().iter() {
            d.borrow_mut().mcu_reset();
        }
    }
}

/// Quadrature levels (A, B) in clockwise order; at rest (detent) both
/// contacts are open and the module's pull-ups hold the lines high.
const QUADRATURE: [(bool, bool); 4] = [(true, true), (false, true), (false, false), (true, false)];
/// Transitions per detent (one full quadrature cycle).
pub const TRANSITIONS_PER_DETENT: i32 = 4;
/// Time between two transitions while the knob turns.
const TRANSITION_NS: u64 = 1_000_000;

/// Mechanical rotary encoder with contacts to GND on two pins (A, B) and
/// pull-ups on the module. Clockwise, A falls before B: a timer in encoder
/// mode counts up.
pub struct RotaryEncoder {
    gpio: SharedGpio,
    a: (usize, u32),
    b: (usize, u32),
    state: usize,
    /// Transitions still to do (positive: clockwise).
    pending: i32,
    /// Cycle of the next transition (0: as soon as possible).
    next_at: Option<u64>,
}

impl RotaryEncoder {
    pub fn new(gpio: SharedGpio, a: (char, u32), b: (char, u32)) -> Self {
        let mut e = Self { gpio, a: (port(a.0), a.1), b: (port(b.0), b.1), state: 0, pending: 0, next_at: None };
        e.drive(0);
        e
    }

    /// Turns the knob by `detents` (negative: counter-clockwise).
    pub fn rotate(&mut self, detents: i32) {
        self.pending += detents * TRANSITIONS_PER_DETENT;
        if self.pending != 0 && self.next_at.is_none() {
            self.next_at = Some(0);
        }
    }

    /// Detents still to turn.
    pub fn turning(&self) -> bool {
        self.pending != 0
    }

    fn drive(&mut self, cycle: u64) {
        let (a, b) = QUADRATURE[self.state];
        let mut g = self.gpio.borrow_mut();
        g.drive_at(self.a.0, self.a.1, Some(a), cycle);
        g.drive_at(self.b.0, self.b.1, Some(b), cycle);
    }
}

impl PinDevice for RotaryEncoder {
    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        while let Some(at) = self.next_at
            && at <= now
        {
            let at = if at == 0 { now } else { at };
            self.state = (self.state as i32 + self.pending.signum()).rem_euclid(4) as usize;
            self.pending -= self.pending.signum();
            self.drive(at);
            self.next_at = (self.pending != 0).then(|| at + ctx.clock.ns_to_cycles(TRANSITION_NS).max(1));
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.next_at
    }
}
