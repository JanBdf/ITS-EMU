//! GPIO ports A–K.
//!
//! All ports share a [`GpioBank`] so that board devices can observe output
//! levels (LEDs, chip selects) and drive input levels (buttons, sensors).

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

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
    /// Level read from inputs that are neither driven nor pulled (floating
    /// pins keep a board-specific level, see the board crate).
    pub float_level: u16,
    /// Pins that exist in the package (others always read 0).
    pub bonded: u16,
    /// Number of rising edges per pin caused by output writes. Lets devices
    /// detect pulses (e.g. chip-select frames) between two of their accesses.
    pub rises: [u32; 16],
    /// Pins whose edges EXTI listens to, edges seen since EXTI last looked,
    /// and the cycle at which the first of them reaches EXTI.
    pub exti_watch: u16,
    pub exti_rise: u16,
    pub exti_fall: u16,
    pub exti_due: u64,
    /// Pins timer channels capture from, and their edges not yet captured;
    /// time (ps) of the first of them if known (0: when the timer looks).
    pub cap_watch: u16,
    pub cap_rise: u16,
    pub cap_fall: u16,
    pub cap_ps: [u64; 16],
    /// Pins whose edges are logged in order (timer encoder inputs, devices)
    /// and the log: (pin, new level, cycle).
    pub log_watch: u16,
    pub edge_log: Vec<(u8, bool, u64)>,
    /// Levels that peripherals drive on the pins, per alternate function
    /// (timer outputs); they apply while the pin selects that function.
    af_drive: [[Option<bool>; 16]; 16],
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
        // STM32F429ZI (LQFP144): ports A..G complete; PH0/PH1 are the HSE
        // clock input from the ST-LINK; ports I..K are not bonded out.
        let bonded = match port {
            0..=6 => 0xFFFF,
            _ => 0,
        };
        Self { moder, ospeedr, pupdr, bonded, ..Default::default() }
    }

    pub fn mode(&self, pin: u32) -> PinMode {
        match (self.moder >> (2 * pin)) & 3 {
            0 => PinMode::Input,
            1 => PinMode::Output,
            2 => PinMode::Alternate,
            _ => PinMode::Analog,
        }
    }

    /// Alternate function selected for a pin (AFRL/AFRH).
    pub fn af(&self, pin: u32) -> u32 {
        (self.afr[(pin / 8) as usize] >> (4 * (pin % 8))) & 0xF
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
        if self.bonded & bit as u16 == 0 {
            return false;
        }
        let ext = (self.ext_mask & bit as u16 != 0).then_some(self.ext_level & bit as u16 != 0);
        let pulled = || match self.pull(pin) {
            Pull::Up => true,
            Pull::Down => false,
            Pull::None => self.float_level & bit as u16 != 0,
        };
        let open_drain = self.otyper & bit != 0;
        match self.mode(pin) {
            // Without a peripheral driving it, an alternate function pin
            // reads its idle level (pull or floating level).
            PinMode::Alternate => match self.af_drive[pin as usize][self.af(pin) as usize] {
                Some(high) if !(open_drain && high) => high,
                _ => ext.unwrap_or_else(pulled),
            },
            PinMode::Output => {
                let driven_high = self.odr & bit != 0;
                if open_drain && driven_high { ext.unwrap_or_else(pulled) } else { driven_high }
            }
            PinMode::Input => ext.unwrap_or_else(pulled),
            PinMode::Analog => false,
        }
    }

    /// Accounts the time since the last change to the pins that are high.
    fn accumulate(&mut self, now: u64) {
        // edges of peripheral outputs may carry a time before the last call
        let now = now.max(self.duty_last);
        let dt = now - self.duty_last;
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

    /// Runs `f` and records the edges it causes on pins EXTI watches; they
    /// reach EXTI at cycle `due`.
    fn tracked<R>(&mut self, due: u64, f: impl FnOnce(&mut Self) -> R) -> R {
        self.tracked_at(due, 0, f)
    }

    /// `tracked` for edges at a known time `edge_ps` (for input capture).
    fn tracked_at<R>(&mut self, due: u64, edge_ps: u64, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.exti_watch | self.cap_watch | self.log_watch == 0 {
            return f(self);
        }
        let before = self.idr() as u16;
        let r = f(self);
        let after = self.idr() as u16;
        for pin in 0..16u8 {
            if (before ^ after) & self.log_watch & (1 << pin) != 0 {
                self.edge_log.push((pin, after & (1 << pin) != 0, due));
            }
        }
        let pending = self.cap_rise | self.cap_fall;
        self.cap_rise |= after & !before & self.cap_watch;
        self.cap_fall |= before & !after & self.cap_watch;
        for pin in 0..16 {
            if (self.cap_rise | self.cap_fall) & !pending & (1 << pin) != 0 {
                self.cap_ps[pin] = edge_ps;
            }
        }
        let rise = after & !before & self.exti_watch;
        let fall = before & !after & self.exti_watch;
        if rise | fall != 0 {
            if self.exti_rise | self.exti_fall == 0 {
                self.exti_due = due;
            }
            self.exti_rise |= rise;
            self.exti_fall |= fall;
        }
        r
    }

    /// Cycle at which pending edges reach EXTI, if there are any.
    pub fn exti_due(&self) -> Option<u64> {
        (self.exti_rise | self.exti_fall != 0).then_some(self.exti_due)
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
        self.drive_at(port, pin, level, 0);
    }

    /// [`Self::drive`] at a known cycle (edges are logged with it).
    pub fn drive_at(&mut self, port: usize, pin: u32, level: Option<bool>, cycle: u64) {
        let bit = 1u16 << pin;
        self.ports[port].tracked(cycle, |p| match level {
            Some(l) => {
                p.ext_mask |= bit;
                if l {
                    p.ext_level |= bit;
                } else {
                    p.ext_level &= !bit;
                }
            }
            None => p.ext_mask &= !bit,
        });
    }

    /// Takes the logged edges of the pins in `mask`, in order.
    pub fn take_edges(&mut self, port: usize, mask: u16) -> Vec<(u8, bool, u64)> {
        let log = &mut self.ports[port].edge_log;
        let (taken, kept) = std::mem::take(log).into_iter().partition(|&(pin, _, _)| mask & (1 << pin) != 0);
        *log = kept;
        taken
    }

    /// When the earliest edge on pins watched by EXTI reaches EXTI.
    pub fn exti_due(&self) -> Option<u64> {
        self.ports.iter().filter_map(PortState::exti_due).min()
    }

    /// A peripheral output (alternate function `af`) drives a pin to `level`
    /// (`None`: released) at cycle `due` = time `edge_ps`.
    pub fn drive_af(&mut self, port: usize, pin: u32, af: u32, level: Option<bool>, due: u64, edge_ps: u64) {
        let p = &mut self.ports[port];
        if p.af_drive[pin as usize][af as usize] == level {
            return;
        }
        p.accumulate(due);
        p.tracked_at(due, edge_ps, |p| p.af_drive[pin as usize][af as usize] = level);
    }

    pub fn level(&self, port: usize, pin: u32) -> bool {
        self.ports[port].level(pin)
    }

    /// Sets the levels that floating inputs (no pull, not driven) of `port` read.
    pub fn set_floating_levels(&mut self, port: usize, levels: u16) {
        self.ports[port].tracked(0, |p| p.float_level = levels);
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

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        u64::MAX
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
        // Edges pass a synchronizer in the APB2 clock domain of EXTI: 26
        // instead of 20 cycles from a BSRR write until a polling loop sees
        // EXTI->PR set at 180/90 MHz.
        let c = self.clocks.get();
        let due = now + 4 * u64::from(c.hclk / c.pclk2.max(1));
        let mut bank = self.bank.borrow_mut();
        bank.ports[self.index].tracked(due, |p| match off & !3 {
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
        });
    }

    fn next_event(&self) -> Option<u64> {
        // ends the slice so that EXTI sees the edge before the next instruction
        let p = &self.bank.borrow().ports[self.index];
        // edges for timer inputs are captured (and logged edges taken) at once
        if p.cap_rise | p.cap_fall != 0 || !p.edge_log.is_empty() { Some(0) } else { p.exti_due() }
    }

    fn reset(&mut self) {
        let mut bank = self.bank.borrow_mut();
        let old = &bank.ports[self.index];
        let (ext_mask, ext_level, float_level, watch, cap, log) =
            (old.ext_mask, old.ext_level, old.float_level, old.exti_watch, old.cap_watch, old.log_watch);
        bank.ports[self.index] = PortState::reset_for(self.index);
        // External drivers (buttons held down, ...) survive a reset of the MCU;
        // the EXTI watch mask belongs to EXTI.
        let new = &mut bank.ports[self.index];
        (new.ext_mask, new.ext_level, new.float_level, new.exti_watch, new.cap_watch, new.log_watch) =
            (ext_mask, ext_level, float_level, watch, cap, log);
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
