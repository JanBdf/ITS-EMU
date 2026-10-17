//! Independent watchdog: resets the MCU when it is not refreshed in time.
//!
//! Clocked by the LSI, measured at about 34 kHz on the board (timeout of the
//! reset values, 4096 × 4 LSI periods: 485 ms; with PR = 2: 1.92 s). PR and
//! RLR changes take about 5 LSI periods (SR.PVU/RVU) and apply at the next
//! reload (refresh), as on the board.

use emu_core::bus::extract_read;
use emu_core::{Clock, DeviceCtx, Peripheral, ResetKind};

const LSI_HZ: u64 = 34_000;
const KEY_UNLOCK: u32 = 0x5555;
const KEY_REFRESH: u32 = 0xAAAA;
const KEY_START: u32 = 0xCCCC;

#[derive(Default)]
pub struct Iwdg {
    unlocked: bool,
    /// Register values and the ones the counter uses.
    pr: u32,
    rlr: u32,
    active_pr: u32,
    active_rlr: u32,
    /// Cycle at which a PR/RLR update has reached the watchdog.
    update_done: Option<u64>,
    /// Cycle at which the counter reaches 0 (watchdog running).
    deadline: Option<u64>,
}

impl Iwdg {
    pub fn new() -> Self {
        Self { rlr: 0xFFF, active_rlr: 0xFFF, ..Default::default() }
    }

    fn lsi_cycles(ctx: &DeviceCtx, n: u64) -> u64 {
        n * u64::from(ctx.clock.cpu_hz()) / LSI_HZ
    }

    fn sync_update(&mut self, now: u64) {
        if self.update_done.is_some_and(|t| t <= now) {
            self.update_done = None;
            (self.active_pr, self.active_rlr) = (self.pr, self.rlr);
        }
    }

    fn reload(&mut self, ctx: &DeviceCtx) {
        let div = 4u64 << self.active_pr.min(6);
        self.deadline = Some(ctx.now() + Self::lsi_cycles(ctx, (u64::from(self.active_rlr) + 1) * div));
    }
}

impl Peripheral for Iwdg {
    fn name(&self) -> &str {
        "IWDG"
    }

    fn read_stable_until(&self, off: u32, _clock: &Clock) -> u64 {
        if off & !3 == 0x0C { self.update_done.unwrap_or(u64::MAX) } else { u64::MAX }
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        self.sync_update(ctx.now());
        let v = match off & !3 {
            0x04 => self.pr,
            0x08 => self.rlr,
            0x0C if self.update_done.is_some() => 3, // PVU | RVU
            _ => 0,
        };
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, _size: u32, value: u32) {
        let now = ctx.now();
        self.sync_update(now);
        match off & !3 {
            0x00 => {
                let key = value & 0xFFFF;
                self.unlocked = key == KEY_UNLOCK;
                match key {
                    KEY_REFRESH if self.deadline.is_some() => self.reload(ctx),
                    KEY_START if self.deadline.is_none() => self.reload(ctx),
                    _ => {}
                }
            }
            0x04 | 0x08 if self.unlocked => {
                if off & !3 == 0x04 {
                    self.pr = value & 7;
                } else {
                    self.rlr = value & 0xFFF;
                }
                self.update_done = Some(now + Self::lsi_cycles(ctx, 5));
            }
            _ => {}
        }
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.sync_update(ctx.now());
        if self.deadline.is_some_and(|t| t <= ctx.now()) {
            self.deadline = None;
            log::info!("IWDG: watchdog reset");
            ctx.request_reset(ResetKind::Watchdog);
        }
    }

    fn next_event(&self) -> Option<u64> {
        [self.deadline, self.update_done].into_iter().flatten().min()
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}
