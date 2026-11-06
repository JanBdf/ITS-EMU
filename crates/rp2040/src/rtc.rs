//! RTC: date and time, one second every CLKDIV_M1 + 1 cycles of clk_rtc.
//!
//! CTRL.LOAD takes the time from SETUP_0/1, RTC_ENABLE runs it (RTC_ACTIVE
//! follows at once). Every fourth year is a leap year unless
//! FORCE_NOTLEAPYEAR; the day of the week counts on with the days. The
//! alarm compares the fields IRQ_SETUP_0/1 enable: INTR is high while
//! they match (the SDK disables the alarm in its handler). Counted lazily
//! from virtual time.

use emu_core::{Clock, DeviceCtx};

use crate::regs::{Block, Op};
use crate::sys::{CLK_RTC, SharedClockTree};

pub const RTC_IRQ: u32 = 25;
const CTRL_ENABLE: u32 = 1 << 0;
const CTRL_ACTIVE: u32 = 1 << 1;
const CTRL_LOAD: u32 = 1 << 4;
const CTRL_NOTLEAP: u32 = 1 << 8;
const MATCH_ENA: u32 = 1 << 28;
const MATCH_ACTIVE: u32 = 1 << 29;
/// The alarm looks this far ahead for its next match (then again later).
const SEARCH_STEPS: usize = 2000;

/// Date and time as the RTC counts them.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub dotw: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
}

impl DateTime {
    /// From the layout of SETUP_0/RTC_1 (date) and SETUP_1/RTC_0 (time).
    fn from_regs(date: u32, time: u32) -> Self {
        Self {
            year: (date >> 12) & 0xFFF,
            month: (date >> 8) & 0xF,
            day: date & 0x1F,
            dotw: (time >> 24) & 7,
            hour: (time >> 16) & 0x1F,
            min: (time >> 8) & 0x3F,
            sec: time & 0x3F,
        }
    }

    fn date_reg(&self) -> u32 {
        (self.year << 12) | (self.month << 8) | self.day
    }

    fn time_reg(&self) -> u32 {
        (self.dotw << 24) | (self.hour << 16) | (self.min << 8) | self.sec
    }

    fn days_in_month(&self, notleap: bool) -> u32 {
        match self.month {
            2 if self.year.is_multiple_of(4) && !notleap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    fn next_day(&mut self, notleap: bool) {
        self.dotw = if self.dotw >= 6 { 0 } else { self.dotw + 1 };
        if self.day >= self.days_in_month(notleap) {
            self.day = 1;
            if self.month >= 12 {
                self.month = 1;
                self.year = (self.year + 1) & 0xFFF;
            } else {
                self.month += 1;
            }
        } else {
            self.day += 1;
        }
    }

    /// Seconds of the day (fields out of range carry at the next step).
    fn seconds_of_day(&self) -> u64 {
        u64::from(self.hour) * 3600 + u64::from(self.min) * 60 + u64::from(self.sec)
    }

    /// `n` seconds later.
    fn add_seconds(&mut self, n: u64, notleap: bool) {
        let mut left = n;
        while left > 0 {
            let sod = self.seconds_of_day();
            if sod + left < 86_400 && self.hour < 24 && self.min < 60 && self.sec < 60 {
                let s = sod + left;
                (self.hour, self.min, self.sec) = ((s / 3600) as u32, (s / 60 % 60) as u32, (s % 60) as u32);
                return;
            }
            // to midnight, one step at a time when a field is out of range
            let step = if self.hour < 24 && self.min < 60 && self.sec < 60 { 86_400 - sod } else { 1 };
            if step == 1 {
                self.tick(notleap);
            } else {
                (self.hour, self.min, self.sec) = (0, 0, 0);
                self.next_day(notleap);
            }
            left -= step.min(left);
        }
    }

    /// One second later.
    fn tick(&mut self, notleap: bool) {
        self.sec += 1;
        if self.sec >= 60 {
            self.sec = 0;
            self.min += 1;
            if self.min >= 60 {
                self.min = 0;
                self.hour += 1;
                if self.hour >= 24 {
                    self.hour = 0;
                    self.next_day(notleap);
                }
            }
        }
    }
}

pub struct Rtc {
    tree: SharedClockTree,
    clkdiv_m1: u32,
    setup: [u32; 2],
    ctrl: u32,
    irq_setup: [u32; 2],
    inte: bool,
    intf: bool,
    /// The time at `base_ps` (running: a second boundary).
    now: DateTime,
    base_ps: u64,
    /// Picoseconds per second of the RTC as a fraction num / hz (running).
    tick: Option<(u128, u128)>,
    /// Cycle of the next alarm change (match begins or ends).
    alarm_at: Option<u64>,
}

impl Rtc {
    pub fn new(tree: SharedClockTree) -> Self {
        Self {
            tree,
            clkdiv_m1: 0,
            setup: [0; 2],
            ctrl: 0,
            irq_setup: [0; 2],
            inte: false,
            intf: false,
            now: DateTime::default(),
            base_ps: 0,
            tick: None,
            alarm_at: None,
        }
    }

    fn notleap(&self) -> bool {
        self.ctrl & CTRL_NOTLEAP != 0
    }

    /// Time of `k` seconds after `base_ps`.
    fn tick_ps(&self, k: u64) -> Option<u64> {
        let (num, hz) = self.tick?;
        Some(self.base_ps + (u128::from(k) * num / hz) as u64)
    }

    /// Counts the seconds up to now.
    fn advance(&mut self, clock: &Clock) {
        let Some((num, hz)) = self.tick else { return };
        let k = (u128::from(clock.time_ps().saturating_sub(self.base_ps)) * hz / num) as u64;
        if k > 0 {
            self.now.add_seconds(k, self.notleap());
            self.base_ps = self.tick_ps(k).unwrap_or(self.base_ps);
        }
    }

    /// Starts or stops counting after CTRL, CLKDIV or the clock changed.
    fn restart(&mut self, clock: &Clock) {
        self.advance(clock);
        let hz = self.tree.borrow().hz(CLK_RTC);
        self.tick = (self.ctrl & CTRL_ENABLE != 0 && hz != 0)
            .then(|| (u128::from(self.clkdiv_m1 + 1) * 1_000_000_000_000, u128::from(hz)));
        if self.tick.is_some() {
            self.base_ps = clock.time_ps();
        }
    }

    /// The alarm's enabled fields match `t`.
    fn matches(&self, t: &DateTime) -> bool {
        let [s0, s1] = self.irq_setup;
        if s0 & MATCH_ENA == 0 {
            return false;
        }
        let want = DateTime::from_regs(s0, s1);
        let fields = [
            (s0 & (1 << 26), t.year, want.year),
            (s0 & (1 << 25), t.month, want.month),
            (s0 & (1 << 24), t.day, want.day),
            (s1 & (1 << 31), t.dotw, want.dotw),
            (s1 & (1 << 30), t.hour, want.hour),
            (s1 & (1 << 29), t.min, want.min),
            (s1 & (1 << 28), t.sec, want.sec),
        ];
        fields.iter().all(|&(on, have, want)| on == 0 || have == want)
    }

    /// Seconds from now to the next change of the match: its end, or the
    /// next second that matches (searching by the largest unit that does
    /// not; `None` beyond the search).
    fn seconds_to_alarm_change(&self) -> Option<u64> {
        if self.irq_setup[0] & MATCH_ENA == 0 {
            return None;
        }
        let now = self.matches(&self.now);
        let notleap = self.notleap();
        let mut t = self.now;
        let mut k = 0u64;
        for _ in 0..SEARCH_STEPS {
            let step = if now || self.matches(&t) {
                1
            } else {
                let [s0, s1] = self.irq_setup;
                let want = DateTime::from_regs(s0, s1);
                let sod = t.seconds_of_day();
                let valid = t.hour < 24 && t.min < 60 && t.sec < 60;
                let day_left = if valid { 86_400 - sod } else { 1 };
                if s0 & (1 << 26) != 0 && t.year != want.year
                    || s0 & (1 << 25) != 0 && t.month != want.month
                    || s0 & (1 << 24) != 0 && t.day != want.day
                    || s1 & (1 << 31) != 0 && t.dotw != want.dotw
                {
                    day_left
                } else if s1 & (1 << 30) != 0 && t.hour != want.hour && valid {
                    3600 - u64::from(t.min) * 60 - u64::from(t.sec)
                } else if s1 & (1 << 29) != 0 && t.min != want.min && valid {
                    60 - u64::from(t.sec)
                } else {
                    1
                }
            };
            t.add_seconds(step, notleap);
            k += step;
            if self.matches(&t) != now {
                return Some(k);
            }
        }
        None
    }

    fn schedule(&mut self, clock: &Clock) {
        self.alarm_at = None;
        if self.tick.is_none() || self.irq_setup[0] & MATCH_ENA == 0 {
            return;
        }
        // nothing found: look again after the seconds searched
        let k = self.seconds_to_alarm_change().unwrap_or(SEARCH_STEPS as u64);
        if let Some(ps) = self.tick_ps(k) {
            self.alarm_at = Some(clock.cycle_at_ps(ps).max(clock.cycles() + 1));
        }
    }

    fn intr(&self) -> bool {
        self.matches(&self.now)
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        ctx.set_irq(RTC_IRQ, (self.intr() || self.intf) && self.inte);
    }

    /// The time the RTC shows now.
    pub fn datetime(&mut self, clock: &Clock) -> DateTime {
        self.advance(clock);
        self.now
    }
}

impl Block for Rtc {
    fn name(&self) -> &str {
        "RTC"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32 {
        self.advance(ctx.clock);
        match off {
            0x00 => self.clkdiv_m1,
            0x04 => self.setup[0],
            0x08 => self.setup[1],
            0x0C => self.ctrl,
            0x10 => self.irq_setup[0] | if self.irq_setup[0] & MATCH_ENA != 0 { MATCH_ACTIVE } else { 0 },
            0x14 => self.irq_setup[1],
            0x18 => self.now.date_reg(),
            0x1C => self.now.time_reg(),
            0x20 => u32::from(self.intr()),
            0x24 => u32::from(self.inte),
            0x28 => u32::from(self.intf),
            0x2C => u32::from((self.intr() || self.intf) && self.inte),
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        self.advance(ctx.clock);
        match off {
            0x00 => {
                self.clkdiv_m1 = op.apply(self.clkdiv_m1, value) & 0xFFFF;
                self.restart(ctx.clock);
            }
            0x04 => self.setup[0] = op.apply(self.setup[0], value) & 0x00FF_FF1F,
            0x08 => self.setup[1] = op.apply(self.setup[1], value) & 0x071F_3F3F,
            0x0C => {
                let v = op.apply(self.ctrl, value);
                if v & CTRL_LOAD != 0 {
                    self.now = DateTime::from_regs(self.setup[0], self.setup[1]);
                    self.base_ps = ctx.clock.time_ps();
                }
                let enable = v & CTRL_ENABLE;
                self.ctrl = (v & (CTRL_ENABLE | CTRL_NOTLEAP)) | if enable != 0 { CTRL_ACTIVE } else { 0 };
                self.restart(ctx.clock);
            }
            0x10 => self.irq_setup[0] = op.apply(self.irq_setup[0], value) & 0x17FF_FF1F,
            0x14 => self.irq_setup[1] = op.apply(self.irq_setup[1], value) & 0xF71F_3F3F,
            0x24 => self.inte = op.apply(u32::from(self.inte), value) & 1 != 0,
            0x28 => self.intf = op.apply(u32::from(self.intf), value) & 1 != 0,
            _ => {}
        }
        self.schedule(ctx.clock);
        self.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.alarm_at.is_some_and(|at| at <= ctx.now()) {
            self.advance(ctx.clock);
            self.schedule(ctx.clock);
            self.sync_irq(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.alarm_at
    }

    fn reset(&mut self) {
        *self = Self::new(self.tree.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::nvic::Nvic;

    /// clk_rtc at 46875 Hz (XOSC / 256), as the SDK sets it up.
    fn rtc() -> Rtc {
        let tree = SharedClockTree::default();
        tree.borrow_mut().clk[CLK_RTC] = (1 << 11 | 3 << 5, 256 << 8); // ENABLE, AUXSRC = XOSC, DIV = 256
        Rtc::new(tree)
    }

    /// rtc_set_datetime() of the SDK.
    fn set(r: &mut Rtc, ctx: &mut DeviceCtx, t: DateTime) {
        r.write(ctx, 0x00, Op::Write, 46_874);
        r.write(ctx, 0x0C, Op::Write, 0);
        r.write(ctx, 0x04, Op::Write, t.date_reg());
        r.write(ctx, 0x08, Op::Write, t.time_reg());
        r.write(ctx, 0x0C, Op::Write, CTRL_LOAD);
        r.write(ctx, 0x0C, Op::Write, CTRL_ENABLE);
    }

    fn dt(year: u32, month: u32, day: u32, dotw: u32, hour: u32, min: u32, sec: u32) -> DateTime {
        DateTime { year, month, day, dotw, hour, min, sec }
    }

    #[test]
    fn counts_seconds_over_a_leap_day() {
        let mut r = rtc();
        assert_eq!(r.tree.borrow().hz(CLK_RTC), 46_875);
        let mut clock = Clock::new(125_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        set(&mut r, &mut ctx, dt(2028, 2, 28, 1, 23, 59, 58));
        assert_ne!(r.read(&mut ctx, 0x0C) & CTRL_ACTIVE, 0);
        ctx.clock.advance(125_000_000 * 3 - 1);
        assert_eq!(r.datetime(ctx.clock), dt(2028, 2, 29, 2, 0, 0, 0), "just under 3 s");
        ctx.clock.advance(1);
        assert_eq!(r.datetime(ctx.clock), dt(2028, 2, 29, 2, 0, 0, 1));
        ctx.clock.advance(125_000_000 * 86_400);
        assert_eq!(r.datetime(ctx.clock), dt(2028, 3, 1, 3, 0, 0, 1));
        assert_eq!(r.read(&mut ctx, 0x18), (2028 << 12) | (3 << 8) | 1);
    }

    #[test]
    fn alarm_interrupt_at_the_matching_second() {
        let mut r = rtc();
        let mut clock = Clock::new(125_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        set(&mut r, &mut ctx, dt(2026, 12, 31, 4, 23, 58, 0));
        // alarm at 00:00:30 on any day
        r.write(&mut ctx, 0x14, Op::Write, (7 << 28) | 30);
        r.write(&mut ctx, 0x10, Op::Write, MATCH_ENA);
        r.write(&mut ctx, 0x24, Op::Write, 1);
        assert_ne!(r.read(&mut ctx, 0x10) & MATCH_ACTIVE, 0);
        let due = r.next_event().unwrap();
        assert_eq!(due, 125_000_000 * 150, "2 min 30 s later");
        ctx.clock.advance(due);
        r.update(&mut ctx);
        assert!(ctx.nvic.is_pending(16 + RTC_IRQ));
        assert_eq!(r.datetime(ctx.clock), dt(2027, 1, 1, 5, 0, 0, 30));
        // the handler disables the alarm
        r.write(&mut ctx, 0x10, Op::Clear, MATCH_ENA);
        assert_eq!(r.read(&mut ctx, 0x2C), 0);
        assert_eq!(r.next_event(), None);
    }
}
