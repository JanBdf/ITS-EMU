//! General-purpose timers (TIM2–TIM5) in up-counting mode.
//!
//! The counter is evaluated lazily from virtual time. Supported: prescaler and
//! auto-reload (with preload), update events (overflow, `EGR.UG`), the update
//! interrupt, `CNT` reads/writes. Capture/compare, down/center-aligned counting
//! and slave modes are not modelled yet (a warning is logged when used).

use std::cell::Cell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::rcc::SharedClocks;

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
    wide: bool,
    bus: ApbBus,
    enable_bit: u32,
    clocks: SharedClocks,
    pwm: SharedPwm,

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
    ccr: [u32; 4],
    or: u32,

    /// Counter value at `base_ps`.
    cnt_base: u32,
    base_ps: u64,
    /// Next update event in CPU cycles (for interrupt scheduling).
    next_event: Option<u64>,
    warned: bool,
}

impl GpTimer {
    pub fn new(name: &'static str, irq: u32, wide: bool, bus: ApbBus, enable_bit: u32, clocks: SharedClocks) -> Self {
        let mut t = Self {
            name,
            irq,
            wide,
            bus,
            enable_bit,
            clocks,
            pwm: SharedPwm::default(),
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
            ccr: [0; 4],
            or: 0,
            cnt_base: 0,
            base_ps: 0,
            next_event: None,
            warned: false,
        };
        t.reset_regs();
        t
    }

    fn reset_regs(&mut self) {
        let max = self.max();
        *self = Self {
            name: self.name,
            irq: self.irq,
            wide: self.wide,
            bus: self.bus,
            enable_bit: self.enable_bit,
            clocks: self.clocks.clone(),
            pwm: self.pwm.clone(),
            cr1: 0,
            cr2: 0,
            smcr: 0,
            dier: 0,
            sr: 0,
            ccmr: [0; 2],
            ccer: 0,
            psc: 0,
            psc_active: 0,
            arr: max,
            arr_active: max,
            ccr: [0; 4],
            or: 0,
            cnt_base: 0,
            base_ps: 0,
            next_event: None,
            warned: false,
        };
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
    }

    fn update_event(&mut self, set_flag: bool) {
        self.psc_active = self.psc;
        self.arr_active = self.arr;
        if set_flag {
            self.sr |= SR_UIF;
        }
        if self.cr1 & CR1_OPM != 0 {
            self.cr1 &= !CR1_CEN;
        }
    }

    /// Processes overflows up to `now_ps`.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now_ps = ctx.clock.time_ps();
        if !self.running() || self.arr_active == 0 {
            self.next_event = None;
            return;
        }
        let (ticks, _) = self.elapsed_ticks(now_ps);
        let to_overflow = u128::from(self.arr_active - self.cnt_base.min(self.arr_active)) + 1;
        if ticks >= to_overflow {
            // Time of the (first) overflow, then restart from 0 there.
            let hz = u128::from(self.kernel_hz().max(1));
            let wrap_ps = self.base_ps + (to_overflow * self.tick_ps_num() / hz) as u64;
            let udis = self.cr1 & CR1_UDIS != 0;
            if !udis {
                self.update_event(true);
            }
            self.base_ps = wrap_ps;
            self.cnt_base = 0;
            if self.running() {
                // Further complete periods with the (now active) settings.
                let (ticks, _) = self.elapsed_ticks(now_ps);
                let wraps = ticks / self.period();
                if wraps > 0 {
                    self.base_ps += (wraps * self.period() * self.tick_ps_num() / hz) as u64;
                    if !udis {
                        self.sr |= SR_UIF;
                    }
                }
            }
        }
        self.schedule(ctx);
    }

    fn schedule(&mut self, ctx: &mut DeviceCtx) {
        self.next_event = None;
        if !self.running() || self.arr_active == 0 || self.dier & DIER_UIE == 0 {
            return;
        }
        let hz = u128::from(self.kernel_hz().max(1));
        let to_overflow = u128::from(self.arr_active - self.cnt_base.min(self.arr_active)) + 1;
        let wrap_ps = self.base_ps as u128 + to_overflow * self.tick_ps_num() / hz;
        let now_ps = u128::from(ctx.clock.time_ps());
        let dt_ps = wrap_ps.saturating_sub(now_ps);
        let dt_cycles = (dt_ps * u128::from(ctx.clock.cpu_hz())).div_ceil(PS_PER_S) as u64;
        self.next_event = Some(ctx.now() + dt_cycles.max(1));
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

    fn next_overflow_cycle(&self, clock: &Clock) -> u64 {
        if self.arr_active == 0 {
            return u64::MAX;
        }
        let to_overflow = u128::from(self.arr_active - self.cnt_base.min(self.arr_active)) + 1;
        self.cycle_of_tick(to_overflow, clock)
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        ctx.set_irq(self.irq, self.sr & self.dier & 0x5F != 0);
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
            0x34 => self.ccr[0],
            0x38 => self.ccr[1],
            0x3C => self.ccr[2],
            0x40 => self.ccr[3],
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
            0x10 => self.next_overflow_cycle(clock),
            _ => u64::MAX,
        }
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        self.advance(ctx);
        self.sync_irq(ctx);
        extract_read(self.reg(off & !3, ctx.clock.time_ps()), off, size)
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
                self.cr1 = v & 0x3FF;
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
            0x0C => {
                self.dier = v & 0x5F5F;
                if v & 0x1E != 0 {
                    self.warn_unsupported("capture/compare interrupts");
                }
            }
            0x10 => self.sr &= v, // rc_w0
            0x14 => {
                if v & 1 != 0 {
                    // UG: re-initialise counter and prescaler, update shadow registers.
                    let flag = self.cr1 & CR1_URS == 0;
                    self.update_event(flag);
                    self.rebase(now_ps, 0);
                    self.base_ps = now_ps;
                }
            }
            0x18 => self.ccmr[0] = v,
            0x1C => self.ccmr[1] = v,
            0x20 => self.ccer = v,
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
            0x34 => self.ccr[0] = v & mask,
            0x38 => self.ccr[1] = v & mask,
            0x3C => self.ccr[2] = v & mask,
            0x40 => self.ccr[3] = v & mask,
            0x50 => self.or = v,
            _ => {}
        }
        self.schedule(ctx);
        self.sync_irq(ctx);
        self.update_pwm();
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.next_event.is_some_and(|t| ctx.now() >= t) {
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
