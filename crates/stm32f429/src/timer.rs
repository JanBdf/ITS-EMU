//! Timers (TIM1–TIM14) in up-counting mode.
//!
//! The counter is evaluated lazily from virtual time. Supported: prescaler and
//! auto-reload (with preload), update events (overflow, `EGR`), the
//! repetition counter of TIM1/TIM8, output compare flags and interrupts, PWM
//! duty cycles, `CNT` reads/writes, input capture of edges that drive the
//! channel pins from outside (buttons, analog inputs; with prescaler,
//! polarity and overcapture). Timer outputs on pins, down/center-aligned
//! counting and slave modes are not modelled (a warning is logged when used).

use std::cell::Cell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::gpio::{PinMode, SharedGpio, port};
use crate::rcc::SharedClocks;

/// Cycle of the last trigger output (TRGO) of TIM2, TIM3 and TIM8 (index
/// `TRGO_TIM2` ...), for the ADCs' external triggers.
pub type SharedTriggers = Rc<Cell<[u64; 3]>>;
pub const TRGO_TIM2: usize = 0;
pub const TRGO_TIM3: usize = 1;
pub const TRGO_TIM8: usize = 2;

/// Pins of the four channels of a timer, and their alternate function.
pub struct ChannelPins {
    pub af: u32,
    pub pins: [&'static [(char, u32)]; 4],
}

/// Duty cycle (0.0–1.0) of each output channel in PWM mode, `None` otherwise.
pub type SharedPwm = Rc<Cell<[Option<f32>; 4]>>;

const PS_PER_S: u128 = 1_000_000_000_000;

const CR1_CEN: u32 = 1 << 0;
const CR1_UDIS: u32 = 1 << 1;
const CR1_URS: u32 = 1 << 2;
const CR1_OPM: u32 = 1 << 3;
const CR1_DIR_CMS: u32 = 0x70;
const CR1_ARPE: u32 = 1 << 7;
const SR_UIF: u32 = 1 << 0;
const DIER_UIE: u32 = 1 << 0;

/// Which APB bus clocks the timer (selects the kernel clock and enable register).
#[derive(Debug, Clone, Copy)]
pub enum ApbBus {
    Apb1,
    Apb2,
}

pub struct GpTimer {
    name: &'static str,
    irq: u32,
    /// Source index of the timer on its (possibly shared) interrupt lines.
    irq_src: u8,
    /// Advanced-control timers (TIM1/TIM8): interrupt line of the
    /// capture/compare events (the update event has its own).
    cc_irq: Option<u32>,
    wide: bool,
    /// Number of capture/compare channels.
    channels: usize,
    /// Writable bits of CR1 and DIER (measured per timer type).
    cr1_mask: u32,
    dier_mask: u32,
    bus: ApbBus,
    enable_bit: u32,
    clocks: SharedClocks,
    pwm: SharedPwm,
    /// Inputs for capture: the pins and the GPIO bank.
    inputs: Option<(&'static ChannelPins, SharedGpio)>,
    /// Pins watched for capture edges (port, pin).
    watched: Vec<(usize, u32)>,
    /// Edges counted by the input prescalers.
    ic_events: [u32; 4],
    /// Trigger output to the ADCs.
    trgo: Option<(SharedTriggers, usize)>,

    cr1: u32,
    cr2: u32,
    smcr: u32,
    dier: u32,
    sr: u32,
    ccmr: [u32; 2],
    ccer: u32,
    psc: u32,
    psc_active: u32,
    arr: u32,
    arr_active: u32,
    rcr: u32,
    /// Overflows left until the next update event (repetition counter).
    rep_left: u32,
    ccr: [u32; 4],
    bdtr: u32,
    dcr: u32,
    or: u32,

    /// Counter value at `base_ps`.
    cnt_base: u32,
    base_ps: u64,
    /// Ticks after `base_ps` up to which compare matches have been flagged.
    checked: u128,
    /// Next update or compare interrupt in CPU cycles.
    next_event: Option<u64>,
    warned: bool,
}

impl GpTimer {
    pub fn new(name: &'static str, irq: u32, wide: bool, bus: ApbBus, enable_bit: u32, clocks: SharedClocks) -> Self {
        let mut t = Self {
            name,
            irq,
            irq_src: 0,
            cc_irq: None,
            wide,
            channels: 4,
            cr1_mask: 0x3FF,
            dier_mask: 0x5F5F,
            bus,
            enable_bit,
            clocks,
            pwm: SharedPwm::default(),
            inputs: None,
            watched: Vec::new(),
            ic_events: [0; 4],
            trgo: None,
            cr1: 0,
            cr2: 0,
            smcr: 0,
            dier: 0,
            sr: 0,
            ccmr: [0; 2],
            ccer: 0,
            psc: 0,
            psc_active: 0,
            arr: 0,
            arr_active: 0,
            rcr: 0,
            rep_left: 0,
            ccr: [0; 4],
            bdtr: 0,
            dcr: 0,
            or: 0,
            cnt_base: 0,
            base_ps: 0,
            checked: 0,
            next_event: None,
            warned: false,
        };
        t.reset_regs();
        t
    }

    /// Advanced-control timer whose capture/compare events go to `cc_irq`.
    pub fn advanced(mut self, cc_irq: u32) -> Self {
        self.cc_irq = Some(cc_irq);
        self.dier_mask = 0xFFFF;
        self
    }

    /// Basic (0 channels) or small general-purpose timer (TIM9..14).
    pub fn with_channels(mut self, n: usize) -> Self {
        self.channels = n;
        (self.cr1_mask, self.dier_mask) = match n {
            0 => (0x8F, 0x4141),
            1 => (0x38F, 0x03),
            2 => (0x38F, 0x47),
            _ => (0x3FF, 0x5F5F),
        };
        self
    }

    /// Trigger output (master mode "update") for the ADCs.
    pub fn with_trgo(mut self, triggers: SharedTriggers, index: usize) -> Self {
        self.trgo = Some((triggers, index));
        self
    }

    /// TRGO is the update event (CR2.MMS = 010) and someone may listen.
    fn trgo_on_update(&self) -> bool {
        self.trgo.is_some() && (self.cr2 >> 4) & 7 == 2
    }

    fn emit_trgo(&self, cycle: u64) {
        if let Some((t, i)) = &self.trgo {
            let mut v = t.get();
            v[*i] = cycle;
            t.set(v);
        }
    }

    /// Channel inputs for input capture.
    pub fn with_inputs(mut self, pins: &'static ChannelPins, gpio: SharedGpio) -> Self {
        self.inputs = Some((pins, gpio));
        self
    }

    /// The timer is source `src` of an interrupt line shared with another timer.
    pub fn shared_irq(mut self, src: u8) -> Self {
        self.irq_src = src;
        self
    }

    fn reset_regs(&mut self) {
        let max = self.max();
        (self.cr1, self.cr2, self.smcr, self.dier, self.sr, self.ccer) = (0, 0, 0, 0, 0, 0);
        (self.ccmr, self.ccr) = ([0; 2], [0; 4]);
        (self.psc, self.psc_active, self.arr, self.arr_active) = (0, 0, max, max);
        (self.rcr, self.rep_left, self.bdtr, self.dcr, self.or) = (0, 0, 0, 0, 0);
        (self.cnt_base, self.base_ps, self.checked, self.next_event, self.warned) = (0, 0, 0, None, false);
        self.ic_events = [0; 4];
        self.update_watch();
    }

    /// Capture mode of channel `ch`: the channel whose pin it captures (TI1..TI4).
    fn capture_source(&self, ch: usize) -> Option<usize> {
        if ch >= self.channels || self.ccer & (1 << (4 * ch)) == 0 {
            return None;
        }
        match (self.ccmr[ch / 2] >> (8 * (ch % 2))) & 3 {
            1 => Some(ch),
            2 => Some(ch ^ 1),
            _ => None,
        }
    }

    /// Tells the GPIO bank which pins to report edges of.
    fn update_watch(&mut self) {
        let Some((pins, gpio)) = &self.inputs else { return };
        let mut g = gpio.borrow_mut();
        for &(p, pin) in &self.watched {
            let ps = &mut g.ports[p];
            ps.cap_watch &= !(1 << pin);
            ps.cap_rise &= !(1 << pin);
            ps.cap_fall &= !(1 << pin);
        }
        self.watched.clear();
        for ch in 0..4 {
            let Some(src) = self.capture_source(ch) else { continue };
            for &(p, pin) in pins.pins[src] {
                g.ports[port(p)].cap_watch |= 1 << pin;
                self.watched.push((port(p), pin));
            }
        }
    }

    /// Captures the counter on the edges the watched pins had since the last call.
    fn take_captures(&mut self, now_ps: u64) {
        let Some((pins, gpio)) = &self.inputs else { return };
        if self.watched.is_empty() {
            return;
        }
        let mut g = gpio.borrow_mut();
        // edges per channel input (rising, falling), from pins routed to the timer
        let mut edges = [(false, false); 4];
        for (src, list) in pins.pins.iter().enumerate() {
            for &(p, pin) in *list {
                let ps = &mut g.ports[port(p)];
                let bit = 1u16 << pin;
                let routed = ps.mode(pin) == PinMode::Alternate
                    && (ps.afr[(pin / 8) as usize] >> (4 * (pin % 8))) & 0xF == pins.af;
                if routed {
                    edges[src].0 |= ps.cap_rise & bit != 0;
                    edges[src].1 |= ps.cap_fall & bit != 0;
                }
                ps.cap_rise &= !bit;
                ps.cap_fall &= !bit;
            }
        }
        drop(g);
        let cnt = self.cnt(now_ps);
        for ch in 0..4 {
            let Some(src) = self.capture_source(ch) else { continue };
            let (rise, fall) = edges[src];
            let (p, np) = (self.ccer & (2 << (4 * ch)) != 0, self.ccer & (8 << (4 * ch)) != 0);
            let hit = match (p, np) {
                (false, false) => rise,
                (true, false) => fall,
                _ => rise || fall,
            };
            if !hit {
                continue;
            }
            let psc = (self.ccmr[ch / 2] >> (8 * (ch % 2) + 2)) & 3;
            self.ic_events[ch] += 1;
            if self.ic_events[ch] < 1 << psc {
                continue;
            }
            self.ic_events[ch] = 0;
            if self.sr & (2 << ch) != 0 {
                self.sr |= 0x200 << ch; // CCxOF: overcapture
            }
            self.sr |= 2 << ch;
            self.ccr[ch] = cnt;
        }
    }

    /// Handle to the PWM duty cycles of the output channels.
    pub fn pwm(&self) -> SharedPwm {
        self.pwm.clone()
    }

    fn update_pwm(&self) {
        let mut duty = [None; 4];
        if self.running() {
            for (ch, d) in duty.iter_mut().enumerate() {
                let ccmr = self.ccmr[ch / 2] >> (8 * (ch % 2));
                let mode = (ccmr >> 4) & 7;
                let enabled = self.ccer & (1 << (4 * ch)) != 0;
                if !enabled || !matches!(mode, 6 | 7) {
                    continue;
                }
                let mut v = (f64::from(self.ccr[ch]) / (f64::from(self.arr_active) + 1.0)).clamp(0.0, 1.0) as f32;
                if mode == 7 {
                    v = 1.0 - v; // PWM mode 2
                }
                if self.ccer & (2 << (4 * ch)) != 0 {
                    v = 1.0 - v; // active low
                }
                *d = Some(v);
            }
        }
        self.pwm.set(duty);
    }

    fn max(&self) -> u32 {
        if self.wide { u32::MAX } else { 0xFFFF }
    }

    fn kernel_hz(&self) -> u32 {
        let c = self.clocks.get();
        match self.bus {
            ApbBus::Apb1 => c.apb1_timer,
            ApbBus::Apb2 => c.apb2_timer,
        }
    }

    fn clock_enabled(&self) -> bool {
        let c = self.clocks.get();
        let enr = match self.bus {
            ApbBus::Apb1 => c.apb1enr,
            ApbBus::Apb2 => c.apb2enr,
        };
        enr & (1 << self.enable_bit) != 0
    }

    fn running(&self) -> bool {
        self.cr1 & CR1_CEN != 0
    }

    /// Duration of one counter tick in picoseconds (as a rational: ps * hz).
    fn tick_ps_num(&self) -> u128 {
        PS_PER_S * (u128::from(self.psc_active) + 1)
    }

    /// Ticks elapsed since `base_ps` and the remainder in "ps * hz" units.
    fn elapsed_ticks(&self, now_ps: u64) -> (u128, u128) {
        if !self.running() {
            return (0, 0);
        }
        let dt = u128::from(now_ps.saturating_sub(self.base_ps)) * u128::from(self.kernel_hz());
        let tick = self.tick_ps_num();
        (dt / tick, dt % tick)
    }

    fn period(&self) -> u128 {
        u128::from(self.arr_active) + 1
    }

    /// Current counter value without changing state.
    fn cnt(&self, now_ps: u64) -> u32 {
        let (ticks, _) = self.elapsed_ticks(now_ps);
        if self.arr_active == 0 {
            return self.cnt_base;
        }
        let pos = u128::from(self.cnt_base) + ticks;
        if pos <= u128::from(self.arr_active) {
            pos as u32
        } else {
            ((pos - u128::from(self.arr_active) - 1) % self.period()) as u32
        }
    }

    /// Re-anchors the lazy counter at `now_ps` with value `cnt`, keeping the
    /// fractional tick so that time does not drift.
    fn rebase(&mut self, now_ps: u64, cnt: u32) {
        let (_, rem) = self.elapsed_ticks(now_ps);
        let hz = u128::from(self.kernel_hz().max(1));
        self.base_ps = now_ps - (rem / hz) as u64;
        self.cnt_base = cnt;
        self.checked = 0;
    }

    fn update_event(&mut self, set_flag: bool) {
        self.psc_active = self.psc;
        self.arr_active = self.arr;
        self.rep_left = self.rcr;
        if set_flag {
            self.sr |= SR_UIF;
        }
        if self.cr1 & CR1_OPM != 0 {
            self.cr1 &= !CR1_CEN;
        }
    }

    /// `n` counter overflows: whether one of them is an update event (the
    /// repetition counter of advanced timers skips `RCR` overflows each time).
    fn overflows(&mut self, n: u128) -> bool {
        let left = u128::from(self.rep_left);
        if n <= left {
            self.rep_left -= n as u32;
            return false;
        }
        let period = u128::from(self.rcr) + 1;
        self.rep_left = self.rcr - ((n - left - 1) % period) as u32;
        true
    }

    /// Channel `ch` is an output compare channel (CCxS = 00).
    fn compares(&self, ch: usize) -> bool {
        ch < self.channels && (self.ccmr[ch / 2] >> (8 * (ch % 2))) & 3 == 0
    }

    /// Counter value at which channel `ch` matches: CCR, or 0 (at the
    /// overflow) if CCR is above ARR, as measured on the board.
    fn match_value(&self, ch: usize) -> u32 {
        if self.ccr[ch] > self.arr_active { 0 } else { self.ccr[ch] }
    }

    /// Flags the channels whose match value the counter reaches when it goes
    /// from `from` (exclusive) to `to` (inclusive) without overflowing; an
    /// overflow is `to` = ARR + 1, which reaches 0.
    fn flag_matches(&mut self, from: u128, to: u128) {
        for ch in 0..4 {
            if !self.compares(ch) {
                continue;
            }
            let m = u128::from(self.match_value(ch));
            let m = if m == 0 { u128::from(self.arr_active) + 1 } else { m };
            if from < m && m <= to {
                self.sr |= 2 << ch;
            }
        }
    }

    /// Processes overflows and compare matches up to `now_ps`.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now_ps = ctx.clock.time_ps();
        self.take_captures(now_ps);
        if !self.running() || self.arr_active == 0 {
            self.next_event = None;
            return;
        }
        let (ticks, _) = self.elapsed_ticks(now_ps);
        let base = u128::from(self.cnt_base.min(self.arr_active));
        let to_overflow = u128::from(self.arr_active) - base + 1;
        if ticks < to_overflow {
            self.flag_matches(base + self.checked, base + ticks);
            self.checked = ticks;
        } else {
            self.flag_matches(base + self.checked, u128::from(self.arr_active) + 1);
            // Time of the (first) overflow, then restart from 0 there.
            let hz = u128::from(self.kernel_hz().max(1));
            let wrap_ps = self.base_ps + (to_overflow * self.tick_ps_num() / hz) as u64;
            let udis = self.cr1 & CR1_UDIS != 0;
            if !udis && self.overflows(1) {
                self.update_event(true);
                if self.trgo_on_update() {
                    self.emit_trgo(ctx.clock.cycle_at_ps(wrap_ps));
                }
            }
            self.base_ps = wrap_ps;
            self.cnt_base = 0;
            self.checked = 0;
            if self.running() {
                // Further complete periods with the (now active) settings.
                let (ticks, _) = self.elapsed_ticks(now_ps);
                let wraps = ticks / self.period();
                if wraps > 0 {
                    self.base_ps += (wraps * self.period() * self.tick_ps_num() / hz) as u64;
                    self.flag_matches(0, self.period());
                    if !udis && self.overflows(wraps) {
                        self.update_event(true);
                    }
                }
                let (ticks, _) = self.elapsed_ticks(now_ps);
                self.flag_matches(0, ticks);
                self.checked = ticks;
            }
        }
        self.schedule(ctx);
    }

    /// Ticks after `base_ps` until the next compare match of an interrupt
    /// enabled channel.
    fn next_match_ticks(&self) -> Option<u128> {
        let base = u128::from(self.cnt_base.min(self.arr_active));
        let period = self.period();
        (0..4)
            .filter(|&ch| self.compares(ch) && self.dier & (2 << ch) != 0)
            .map(|ch| {
                let m = u128::from(self.match_value(ch));
                let m = if m == 0 { period } else { m };
                // first tick after `checked` at which the counter reaches m
                let mut t = if m > base { m - base } else { period - base + m };
                if t <= self.checked {
                    t += (self.checked - t) / period * period + period;
                }
                t
            })
            .min()
    }

    fn schedule(&mut self, ctx: &mut DeviceCtx) {
        self.next_event = None;
        if !self.running() || self.arr_active == 0 {
            return;
        }
        let base = u128::from(self.cnt_base.min(self.arr_active));
        let wake = self.dier & DIER_UIE != 0 || self.trgo_on_update();
        let overflow = wake.then(|| u128::from(self.arr_active) - base + 1);
        let Some(ticks) = [overflow, self.next_match_ticks()].into_iter().flatten().min() else { return };
        self.next_event = Some(self.cycle_of_tick(ticks, ctx.clock).max(ctx.now() + 1));
    }

    /// First cycle at which `ticks` ticks after `base_ps` have elapsed.
    fn cycle_of_tick(&self, ticks: u128, clock: &Clock) -> u64 {
        let hz = u128::from(self.kernel_hz().max(1));
        let ps = u128::from(self.base_ps) + (ticks * self.tick_ps_num()).div_ceil(hz);
        clock.cycle_at_ps(u64::try_from(ps).unwrap_or(u64::MAX))
    }

    fn next_tick_cycle(&self, clock: &Clock) -> u64 {
        let (ticks, _) = self.elapsed_ticks(clock.time_ps());
        self.cycle_of_tick(ticks + 1, clock)
    }

    /// Next cycle at which SR may change (overflow or compare match).
    fn next_flag_cycle(&self, clock: &Clock) -> u64 {
        if self.arr_active == 0 {
            return u64::MAX;
        }
        let base = u128::from(self.cnt_base.min(self.arr_active));
        let overflow = u128::from(self.arr_active) - base + 1;
        let mut ticks = overflow;
        for ch in (0..4).filter(|&ch| self.compares(ch)) {
            let m = u128::from(self.match_value(ch));
            if m > base + self.checked {
                ticks = ticks.min(m - base);
            }
        }
        self.cycle_of_tick(ticks, clock)
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        let active = self.sr & self.dier;
        match self.cc_irq {
            Some(cc) => {
                ctx.set_irq_source(self.irq, self.irq_src, active & SR_UIF != 0);
                ctx.set_irq(cc, active & 0x1E != 0);
            }
            None => ctx.set_irq_source(self.irq, self.irq_src, active & 0x5F != 0),
        }
    }

    fn reg(&self, off: u32, now_ps: u64) -> u32 {
        match off {
            0x00 => self.cr1,
            0x04 => self.cr2,
            0x08 => self.smcr,
            0x0C => self.dier,
            0x10 => self.sr,
            0x18 => self.ccmr[0],
            0x1C => self.ccmr[1],
            0x20 => self.ccer,
            0x24 => self.cnt(now_ps),
            0x28 => self.psc,
            0x2C => self.arr,
            0x30 => self.rcr,
            0x34 => self.ccr[0],
            0x38 => self.ccr[1],
            0x3C => self.ccr[2],
            0x40 => self.ccr[3],
            0x44 => self.bdtr,
            0x48 => self.dcr,
            0x50 => self.or,
            _ => 0,
        }
    }

    fn warn_unsupported(&mut self, what: &str) {
        if !self.warned {
            self.warned = true;
            log::warn!("{}: {what} is not emulated yet", self.name);
        }
    }
}

impl Peripheral for GpTimer {
    fn name(&self) -> &str {
        self.name
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        // CNT changes with every tick, SR (UIF) at the next overflow.
        if !self.running() {
            return u64::MAX;
        }
        match off & !3 {
            0x24 => self.next_tick_cycle(clock),
            0x10 => self.next_flag_cycle(clock),
            _ => u64::MAX,
        }
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        self.advance(ctx);
        let v = self.reg(off & !3, ctx.clock.time_ps());
        if let 0x34..=0x40 = off & !3 {
            // reading a captured value clears its flag
            let ch = ((off & !3) - 0x34) as usize / 4;
            if self.capture_source(ch).is_some() {
                self.sr &= !(2 << ch);
            }
        }
        self.sync_irq(ctx);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            log::warn!("{}: write while its clock is disabled (ignored)", self.name);
            return;
        }
        self.advance(ctx);
        let now_ps = ctx.clock.time_ps();
        let aligned = off & !3;
        let v = merge_write(self.reg(aligned, now_ps), off, size, value);
        let mask = self.max();
        match aligned {
            0x00 => {
                let cnt = self.cnt(now_ps);
                self.cr1 = v & self.cr1_mask;
                self.rebase(now_ps, cnt);
                if self.cr1 & CR1_DIR_CMS != 0 {
                    self.warn_unsupported("down/center-aligned counting");
                }
                if self.cr1 & CR1_ARPE == 0 {
                    self.arr_active = self.arr;
                }
            }
            0x04 => self.cr2 = v,
            0x08 => {
                self.smcr = v;
                if v & 7 != 0 {
                    self.warn_unsupported("slave mode");
                }
            }
            0x0C => self.dier = v & self.dier_mask,
            0x10 => self.sr &= v, // rc_w0
            0x14 => {
                if v & 1 != 0 {
                    // UG: re-initialise counter and prescaler, update shadow registers.
                    let flag = self.cr1 & CR1_URS == 0;
                    self.update_event(flag);
                    self.rebase(now_ps, 0);
                    self.base_ps = now_ps;
                    if self.trgo_on_update() {
                        self.emit_trgo(ctx.now()); // UG is an update event for TRGO, too
                    }
                }
                // CCxG, COMG, TG, BG: set their flags
                let channels = (0x1Eu32 >> (4 - self.channels)) & 0x1E;
                self.sr |= v & (channels | 0xE0);
            }
            0x18 | 0x1C => {
                // CCxS can only be changed while the channel is off (CCxE = 0)
                let i = (aligned as usize - 0x18) / 4;
                let mut keep = 0;
                for half in 0..2 {
                    if self.ccer & (1 << (4 * (2 * i + half))) != 0 {
                        keep |= 3 << (8 * half);
                    }
                }
                self.ccmr[i] = (v & !keep) | (self.ccmr[i] & keep);
                self.update_watch();
            }
            0x20 => {
                self.ccer = v;
                self.update_watch();
            }
            0x24 => {
                self.rebase(now_ps, v & mask);
                self.base_ps = now_ps;
            }
            0x28 => self.psc = v & 0xFFFF,
            0x2C => {
                self.arr = v & mask;
                if self.cr1 & CR1_ARPE == 0 {
                    self.arr_active = self.arr;
                }
            }
            // read-only while capturing
            0x34..=0x40 if self.capture_source((aligned - 0x34) as usize / 4).is_none() => {
                self.ccr[(aligned - 0x34) as usize / 4] = v & mask
            }
            0x30 if self.cc_irq.is_some() => self.rcr = v & 0xFF,
            0x44 if self.cc_irq.is_some() => self.bdtr = v,
            0x48 => self.dcr = v & 0x1F1F,
            0x50 => self.or = v,
            _ => {}
        }
        self.schedule(ctx);
        self.sync_irq(ctx);
        self.update_pwm();
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.next_event.is_some_and(|t| ctx.now() >= t) || !self.watched.is_empty() {
            self.advance(ctx);
            self.sync_irq(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.next_event
    }

    fn reset(&mut self) {
        self.reset_regs();
        self.update_pwm();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::{SharedGpio, port};
    use crate::rcc::Clocks;
    use emu_core::nvic::Nvic;

    fn tim2() -> GpTimer {
        let clocks = SharedClocks::default();
        clocks.set(Clocks { apb1_timer: 90_000_000, apb1enr: 1, hclk: 180_000_000, ..Clocks::default() });
        GpTimer::new("TIM2", 28, true, ApbBus::Apb1, 0, clocks)
    }

    #[test]
    fn its_board_init_timer_counts_90_per_us() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        // initTimer(): PSC = 0, ARR = max, CEN
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x28, 4, 0);
            t.write(&mut ctx, 0x2C, 4, 0xFFFF_FFFF);
            t.write(&mut ctx, 0x00, 4, 1);
        }
        clock.advance(180_000_000 / 1000); // 1 ms
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        assert_eq!(t.read(&mut ctx, 0x24, 4), 90_000);
    }

    #[test]
    fn cnt_is_stable_until_the_next_prescaled_tick() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x00, 4, 1);
            t.write(&mut ctx, 0x28, 2, 90 * 10 - 1); // 10 us = 1800 cycles per tick
            t.write(&mut ctx, 0x14, 2, 1); // UG
        }
        for (now, cnt, until) in [(1000, 0, 1800), (1799, 0, 1800), (1800, 1, 3600), (5000, 2, 5400)] {
            clock.advance(now - clock.cycles());
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            assert_eq!(t.read(&mut ctx, 0x24, 4), cnt, "CNT at {now}");
            assert_eq!(t.read_stable_until(0x24, &clock), until, "CNT stable until, at {now}");
            assert_eq!(t.read_stable_until(0x00, &clock), u64::MAX);
        }
        // Stopped timer: constant.
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        t.write(&mut ctx, 0x00, 4, 0);
        assert_eq!(t.read_stable_until(0x24, &clock), u64::MAX);
    }

    #[test]
    fn stoppuhr_prescaler_via_ug_gives_10us_ticks() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x00, 4, 1);
            t.write(&mut ctx, 0x28, 2, 90 * 10 - 1); // STRH PSC
            t.write(&mut ctx, 0x14, 2, 1); // UG
        }
        clock.advance(180_000_000); // 1 s
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        assert_eq!(t.read(&mut ctx, 0x24, 4), 100_000);
        // UG set UIF (URS = 0)
        assert_eq!(t.read(&mut ctx, 0x10, 4) & 1, 1);
    }

    #[test]
    fn input_capture_of_a_button_edge() {
        static PINS: ChannelPins = ChannelPins { af: 3, pins: [&[('F', 6)], &[], &[], &[]] };
        let clocks = SharedClocks::default();
        clocks.set(Clocks { apb2_timer: 180_000_000, apb2enr: 1 << 17, hclk: 180_000_000, ..Clocks::default() });
        let gpio = SharedGpio::default();
        {
            let mut g = gpio.borrow_mut();
            let f = &mut g.ports[port('F')];
            f.moder = 2 << 12; // PF6: alternate function 3 (TIM10_CH1)
            f.afr[0] = 3 << 24;
            g.drive(port('F'), 6, Some(true)); // S6 released
        }
        let mut t = GpTimer::new("TIM10", 25, false, ApbBus::Apb2, 17, clocks)
            .with_channels(1)
            .with_inputs(&PINS, gpio.clone());
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        t.write(&mut ctx, 0x28, 4, 179); // 1 MHz
        t.write(&mut ctx, 0x18, 4, 1); // CC1S = 01: TI1
        t.write(&mut ctx, 0x20, 4, 0b1011); // CC1E, both edges
        t.write(&mut ctx, 0x18, 4, 0); // ignored while CC1E = 1
        assert_eq!(t.read(&mut ctx, 0x18, 4), 1);
        t.write(&mut ctx, 0x14, 4, 1);
        t.write(&mut ctx, 0x10, 4, 0);
        t.write(&mut ctx, 0x00, 4, 1);
        ctx.clock.advance(180 * 250); // 250 us
        gpio.borrow_mut().drive(port('F'), 6, Some(false)); // pressed: falling edge
        t.update(&mut ctx);
        assert_eq!(t.read(&mut ctx, 0x10, 4), 2, "CC1IF");
        ctx.clock.advance(180 * 100);
        gpio.borrow_mut().drive(port('F'), 6, Some(true)); // released before CCR1 was read
        t.update(&mut ctx);
        assert_eq!(t.read(&mut ctx, 0x10, 4), 0x202, "CC1IF + CC1OF");
        assert_eq!(t.read(&mut ctx, 0x34, 4), 350);
        assert_eq!(t.read(&mut ctx, 0x10, 4), 0x200, "reading CCR1 clears CC1IF");
        t.write(&mut ctx, 0x34, 4, 7); // read-only while capturing
        assert_eq!(t.read(&mut ctx, 0x34, 4), 350);
    }

    #[test]
    fn update_interrupt_every_millisecond() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        nvic.set_irq_enabled(28, true);
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x28, 4, 89); // 1 MHz
            t.write(&mut ctx, 0x2C, 4, 999); // 1 kHz
            t.write(&mut ctx, 0x14, 4, 1);
            t.write(&mut ctx, 0x10, 4, 0); // clear UIF from UG
            t.write(&mut ctx, 0x0C, 4, 1); // UIE
            t.write(&mut ctx, 0x00, 4, 1); // CEN
        }
        let ev = t.next_event().unwrap();
        assert_eq!(ev, 180_000);
        clock.advance(ev);
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        t.update(&mut ctx);
        assert!(nvic.is_pending(emu_core::nvic::IRQ0 + 28));
        assert_eq!(t.next_event(), Some(360_000));
    }
}
