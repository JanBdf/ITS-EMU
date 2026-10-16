//! GPIO ports A–K.
//!
//! All ports share a [`GpioBank`] so that board devices can observe output
//! levels (LEDs, chip selects) and drive input levels (buttons, sensors).

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{DeviceCtx, Peripheral};

use crate::rcc::SharedClocks;

pub const NUM_PORTS: usize = 11; // A..K
pub const PORT_STRIDE: u32 = 0x400;
pub const GPIOA_BASE: u32 = 0x4002_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    Input,
    Output,
    Alternate,
    Analog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    None,
    Up,
    Down,
}

#[derive(Debug, Clone, Default)]
pub struct PortState {
    pub moder: u32,
    pub otyper: u32,
    pub ospeedr: u32,
    pub pupdr: u32,
    pub odr: u32,
    pub lckr: u32,
    pub afr: [u32; 2],
    /// Pins driven from outside (buttons, devices) and their levels.
    pub ext_mask: u16,
    pub ext_level: u16,
    /// Number of rising edges per pin caused by output writes. Lets devices
    /// detect pulses (e.g. chip-select frames) between two of their accesses.
    pub rises: [u32; 16],
    /// Time-averaged levels: cycles each pin was high since `duty_since`,
    /// accounted up to `duty_last` (used for LED brightness).
    high_cycles: [u64; 16],
    duty_since: u64,
    duty_last: u64,
}

impl PortState {
    fn reset_for(port: usize) -> Self {
        let (moder, ospeedr, pupdr) = match port {
            0 => (0xA800_0000, 0x0C00_0000, 0x6400_0000),
            1 => (0x0000_0280, 0x0000_00C0, 0x0000_0100),
            _ => (0, 0, 0),
        };
        Self { moder, ospeedr, pupdr, ..Default::default() }
    }

    pub fn mode(&self, pin: u32) -> PinMode {
        match (self.moder >> (2 * pin)) & 3 {
            0 => PinMode::Input,
            1 => PinMode::Output,
            2 => PinMode::Alternate,
            _ => PinMode::Analog,
        }
    }

    pub fn pull(&self, pin: u32) -> Pull {
        match (self.pupdr >> (2 * pin)) & 3 {
            1 => Pull::Up,
            2 => Pull::Down,
            _ => Pull::None,
        }
    }

    /// Electrical level of a pin as seen by the input buffer.
    pub fn level(&self, pin: u32) -> bool {
        let bit = 1u32 << pin;
        let ext = (self.ext_mask & bit as u16 != 0).then_some(self.ext_level & bit as u16 != 0);
        let pulled = || match self.pull(pin) {
            Pull::Up => true,
            Pull::Down => false,
            Pull::None => false,
        };
        match self.mode(pin) {
            PinMode::Output | PinMode::Alternate => {
                let open_drain = self.otyper & bit != 0;
                let driven_high = self.odr & bit != 0;
                if open_drain && driven_high {
                    ext.unwrap_or_else(pulled)
                } else {
                    driven_high
                }
            }
            PinMode::Input => ext.unwrap_or_else(pulled),
            PinMode::Analog => false,
        }
    }

    /// Accounts the time since the last change to the pins that are high.
    fn accumulate(&mut self, now: u64) {
        let dt = now.saturating_sub(self.duty_last);
        if dt > 0 {
            let levels = self.idr();
            for (pin, high) in self.high_cycles.iter_mut().enumerate() {
                if levels & (1 << pin) != 0 {
                    *high += dt;
                }
            }
        }
        self.duty_last = now;
    }

    /// Fraction of time each pin was high since the previous call (or reset).
    fn take_duty(&mut self, now: u64) -> [f32; 16] {
        self.accumulate(now);
        let window = now.saturating_sub(self.duty_since);
        let levels = self.idr();
        let mut duty = [0.0; 16];
        for (pin, d) in duty.iter_mut().enumerate() {
            *d = if window == 0 {
                if levels & (1 << pin) != 0 { 1.0 } else { 0.0 }
            } else {
                (self.high_cycles[pin] as f64 / window as f64) as f32
            };
        }
        self.high_cycles = [0; 16];
        self.duty_since = now;
        duty
    }

    /// Changes ODR and counts rising edges of the resulting pin levels.
    fn set_odr(&mut self, odr: u32, now: u64) {
        self.accumulate(now);
        let before = self.idr();
        self.odr = odr & 0xFFFF;
        let rising = self.idr() & !before;
        for pin in 0..16 {
            if rising & (1 << pin) != 0 {
                self.rises[pin] = self.rises[pin].wrapping_add(1);
            }
        }
    }

    pub fn idr(&self) -> u32 {
        (0..16).filter(|&p| self.level(p)).fold(0, |acc, p| acc | (1 << p))
    }
}

#[derive(Debug, Clone)]
pub struct GpioBank {
    pub ports: [PortState; NUM_PORTS],
}

impl Default for GpioBank {
    fn default() -> Self {
        Self { ports: std::array::from_fn(PortState::reset_for) }
    }
}

impl GpioBank {
    /// Drives an input pin from outside (`Some(level)`) or releases it (`None`).
    pub fn drive(&mut self, port: usize, pin: u32, level: Option<bool>) {
        let p = &mut self.ports[port];
        let bit = 1u16 << pin;
        match level {
            Some(l) => {
                p.ext_mask |= bit;
                if l {
                    p.ext_level |= bit;
                } else {
                    p.ext_level &= !bit;
                }
            }
            None => p.ext_mask &= !bit,
        }
    }

    pub fn level(&self, port: usize, pin: u32) -> bool {
        self.ports[port].level(pin)
    }

    /// Fraction of time (0.0–1.0) each pin of `port` was high since the
    /// previous call. `now` is the current cycle count.
    pub fn take_duty(&mut self, port: usize, now: u64) -> [f32; 16] {
        self.ports[port].take_duty(now)
    }

    /// Rising edges of a pin caused by output writes since reset (wrapping).
    pub fn rises(&self, port: usize, pin: u32) -> u32 {
        self.ports[port].rises[pin as usize]
    }
}

pub type SharedGpio = Rc<RefCell<GpioBank>>;

/// Port index for a port letter ('A' → 0).
pub const fn port(letter: char) -> usize {
    (letter as u8 - b'A') as usize
}

/// Register interface of one GPIO port.
pub struct GpioPort {
    index: usize,
    bank: SharedGpio,
    clocks: SharedClocks,
    warned_clock: bool,
}

impl GpioPort {
    pub fn new(index: usize, bank: SharedGpio, clocks: SharedClocks) -> Self {
        Self { index, bank, clocks, warned_clock: false }
    }

    fn clock_enabled(&mut self) -> bool {
        let en = self.clocks.get().ahb1enr & (1 << self.index) != 0;
        if !en && !self.warned_clock {
            self.warned_clock = true;
            log::warn!(
                "GPIO{} accessed while its clock is disabled (RCC_AHB1ENR bit {}): writes are ignored, reads return 0",
                (b'A' + self.index as u8) as char,
                self.index
            );
        }
        en
    }
}

impl Peripheral for GpioPort {
    fn name(&self) -> &str {
        "GPIO"
    }

    fn read_depends_on_time(&self, _offset: u32) -> bool {
        false
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let bank = self.bank.borrow();
        let p = &bank.ports[self.index];
        let reg = match off & !3 {
            0x00 => p.moder,
            0x04 => p.otyper,
            0x08 => p.ospeedr,
            0x0C => p.pupdr,
            0x10 => p.idr(),
            0x14 => p.odr,
            0x1C => p.lckr,
            0x20 => p.afr[0],
            0x24 => p.afr[1],
            _ => 0, // BSRR reads as 0
        };
        extract_read(reg, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        let now = ctx.now();
        if !self.clock_enabled() {
            return;
        }
        let mut bank = self.bank.borrow_mut();
        let p = &mut bank.ports[self.index];
        match off & !3 {
            0x00 => p.moder = merge_write(p.moder, off, size, value),
            0x04 => p.otyper = merge_write(p.otyper, off, size, value) & 0xFFFF,
            0x08 => p.ospeedr = merge_write(p.ospeedr, off, size, value),
            0x0C => p.pupdr = merge_write(p.pupdr, off, size, value),
            0x14 => {
                let odr = merge_write(p.odr, off, size, value);
                p.set_odr(odr, now);
            }
            0x18 => {
                // Narrow writes only touch their own bits; the rest is zero.
                let v = merge_write(0, off, size, value);
                let set = v & 0xFFFF;
                let reset = v >> 16;
                p.set_odr((p.odr & !reset) | set, now); // set wins over reset
            }
            0x1C => p.lckr = merge_write(p.lckr, off, size, value) & 0x1_FFFF,
            0x20 => p.afr[0] = merge_write(p.afr[0], off, size, value),
            0x24 => p.afr[1] = merge_write(p.afr[1], off, size, value),
            _ => {}
        }
    }

    fn reset(&mut self) {
        let mut bank = self.bank.borrow_mut();
        let ext = (bank.ports[self.index].ext_mask, bank.ports[self.index].ext_level);
        bank.ports[self.index] = PortState::reset_for(self.index);
        // External drivers (buttons held down, ...) survive a reset of the MCU.
        bank.ports[self.index].ext_mask = ext.0;
        bank.ports[self.index].ext_level = ext.1;
        self.warned_clock = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rcc::Clocks;
    use emu_core::Clock;
    use emu_core::nvic::Nvic;

    fn setup(index: usize) -> (GpioPort, SharedGpio) {
        let bank = SharedGpio::default();
        let clocks = SharedClocks::default();
        clocks.set(Clocks { ahb1enr: 0xFF, ..Clocks::default() });
        (GpioPort::new(index, bank.clone(), clocks), bank)
    }

    #[test]
    fn bsrr_halfword_writes_like_lauflicht() {
        let (mut gpio, bank) = setup(port('D'));
        let mut clock = Clock::new(1);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        gpio.write(&mut ctx, 0x00, 4, 0x5555); // PD0..7 output
        gpio.write(&mut ctx, 0x18, 4, 0x00F0); // set 4..7
        gpio.write(&mut ctx, 0x1A, 2, 0x0030); // reset 4,5 via BSRR high half
        assert_eq!(bank.borrow().ports[port('D')].odr, 0x00C0);
        assert_eq!(gpio.read(&mut ctx, 0x10, 4) & 0xFF, 0xC0);
    }

    #[test]
    fn input_with_pullup_and_button() {
        let (mut gpio, bank) = setup(port('F'));
        let mut clock = Clock::new(1);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        gpio.write(&mut ctx, 0x0C, 4, 0x5555); // pull-up on PF0..7
        assert_eq!(gpio.read(&mut ctx, 0x10, 4) & 0xFF, 0xFF);
        bank.borrow_mut().drive(port('F'), 3, Some(false)); // S3 pressed
        assert_eq!(gpio.read(&mut ctx, 0x10, 4) & 0xFF, 0xF7);
    }

    #[test]
    fn duty_cycle_of_a_toggled_pin() {
        let (mut gpio, bank) = setup(port('D'));
        let mut clock = Clock::new(1);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        gpio.write(&mut ctx, 0x00, 4, 0x1); // PD0 output
        bank.borrow_mut().take_duty(port('D'), 0);
        for _ in 0..10 {
            gpio.write(&mut ctx, 0x18, 4, 1); // on for 25 cycles
            ctx.clock.advance(25);
            gpio.write(&mut ctx, 0x18, 4, 1 << 16); // off for 75 cycles
            ctx.clock.advance(75);
        }
        let duty = bank.borrow_mut().take_duty(port('D'), ctx.now());
        assert!((duty[0] - 0.25).abs() < 1e-6, "{}", duty[0]);
        assert_eq!(duty[1], 0.0);
    }

    #[test]
    fn disabled_clock_ignores_writes() {
        let bank = SharedGpio::default();
        let mut gpio = GpioPort::new(port('D'), bank.clone(), SharedClocks::default());
        let mut clock = Clock::new(1);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        gpio.write(&mut ctx, 0x14, 4, 0xFF);
        assert_eq!(bank.borrow().ports[port('D')].odr, 0);
    }
}
