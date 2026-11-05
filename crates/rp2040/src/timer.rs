//! TIMER: 64-bit microsecond counter with four alarms (TIMER_IRQ_0..3).
//! The counter follows virtual time (the SDK runs it from the watchdog tick
//! at 1 MHz).

use emu_core::{Clock, DeviceCtx};

use crate::regs::{Block, Op};

pub struct Timer {
    /// Microseconds added to the time since start (writes to TIMELW/TIMEHW).
    offset: u64,
    /// TIMEHW is latched by a write to TIMELW.
    lw: u32,
    /// TIMEHR is latched by a read of TIMELR.
    hr: u32,
    alarm: [u32; 4],
    armed: u8,
    /// Cycle at which each armed alarm fires.
    fire_at: [Option<u64>; 4],
    intr: u8,
    inte: u8,
    intf: u8,
    pause: bool,
}

pub const TIMER_IRQ_0: u32 = 0;

impl Timer {
    pub fn new() -> Self {
        Self {
            offset: 0,
            lw: 0,
            hr: 0,
            alarm: [0; 4],
            armed: 0,
            fire_at: [None; 4],
            intr: 0,
            inte: 0,
            intf: 0,
            pause: false,
        }
    }

    fn now_us(&self, clock: &Clock) -> u64 {
        (clock.time_ps() / 1_000_000).wrapping_add(self.offset)
    }

    /// Cycle at which the counter reaches `us`.
    fn cycle_at_us(&self, clock: &Clock, us: u64) -> u64 {
        clock.cycle_at_ps(us.wrapping_sub(self.offset).saturating_mul(1_000_000))
    }

    fn schedule(&mut self, clock: &Clock, n: usize) {
        if self.armed & (1 << n) == 0 {
            self.fire_at[n] = None;
            return;
        }
        let now = self.now_us(clock);
        // fires when the low word next equals the alarm value
        let delta = u64::from(self.alarm[n].wrapping_sub(now as u32));
        self.fire_at[n] = Some(self.cycle_at_us(clock, now + delta).max(clock.cycles()));
    }

    fn sync_irqs(&self, ctx: &mut DeviceCtx) {
        let ints = (self.intr | self.intf) & self.inte;
        for n in 0..4 {
            ctx.set_irq(TIMER_IRQ_0 + n, ints & (1 << n) != 0);
        }
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl Block for Timer {
    fn name(&self) -> &str {
        "TIMER"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32 {
        self.update(ctx);
        let now = self.now_us(ctx.clock);
        match off {
            0x08 => self.hr,
            0x0C => {
                self.hr = (now >> 32) as u32;
                now as u32
            }
            0x10..=0x1C => self.alarm[((off - 0x10) / 4) as usize],
            0x20 => u32::from(self.armed),
            0x24 => (now >> 32) as u32,
            0x28 => now as u32,
            0x30 => u32::from(self.pause),
            0x34 => u32::from(self.intr),
            0x38 => u32::from(self.inte),
            0x3C => u32::from(self.intf),
            0x40 => u32::from((self.intr | self.intf) & self.inte),
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        self.update(ctx);
        match off {
            0x00 => {
                // TIMEHW: sets the counter together with the latched low word
                let target = (u64::from(value) << 32) | u64::from(self.lw);
                let since_start = ctx.clock.time_ps() / 1_000_000;
                self.offset = target.wrapping_sub(since_start);
            }
            0x04 => self.lw = value,
            0x10..=0x1C => {
                let n = ((off - 0x10) / 4) as usize;
                self.alarm[n] = op.apply(self.alarm[n], value);
                self.armed |= 1 << n;
                self.schedule(ctx.clock, n);
            }
            0x20 => {
                // ARMED: write 1 to disarm
                self.armed &= !(op.clears(value) as u8 & 0xF);
                for n in 0..4 {
                    self.schedule(ctx.clock, n);
                }
            }
            0x30 => self.pause = op.apply(u32::from(self.pause), value) & 1 != 0,
            0x34 => self.intr &= !(op.clears(value) as u8 & 0xF),
            0x38 => self.inte = op.apply(u32::from(self.inte), value) as u8 & 0xF,
            0x3C => self.intf = op.apply(u32::from(self.intf), value) as u8 & 0xF,
            _ => {}
        }
        self.sync_irqs(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        let mut fired = false;
        for n in 0..4 {
            if let Some(at) = self.fire_at[n]
                && at <= now
            {
                self.fire_at[n] = None;
                self.armed &= !(1 << n);
                self.intr |= 1 << n;
                fired = true;
            }
        }
        if fired {
            self.sync_irqs(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.fire_at.iter().flatten().copied().min()
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        match off {
            // the counter: until the next microsecond
            0x0C | 0x24 | 0x28 => self.cycle_at_us(clock, self.now_us(clock) + 1),
            0x34 | 0x40 => self.next_event().unwrap_or(u64::MAX),
            _ => u64::MAX,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}
