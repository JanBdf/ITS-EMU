//! SysTick timer, evaluated lazily from the cycle counter.

const CSR_ENABLE: u32 = 1 << 0;
const CSR_TICKINT: u32 = 1 << 1;
const CSR_CLKSOURCE: u32 = 1 << 2;
const CSR_COUNTFLAG: u32 = 1 << 16;

/// On STM32F4 the "external" SysTick reference clock is HCLK / 8.
const EXT_CLOCK_DIV: u64 = 8;

#[derive(Debug, Clone, Default)]
pub struct SysTick {
    csr: u32,
    load: u32,
    /// Cycle at which the counter was (virtually) zero before its first reload.
    base: u64,
    /// Number of 1→0 transitions already accounted for since `base`.
    wraps_seen: u64,
    /// Counter value while disabled.
    frozen: u32,
}

impl SysTick {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn enabled(&self) -> bool {
        self.csr & CSR_ENABLE != 0
    }

    fn div(&self) -> u64 {
        if self.csr & CSR_CLKSOURCE != 0 { 1 } else { EXT_CLOCK_DIV }
    }

    fn period(&self) -> u64 {
        u64::from(self.load) + 1
    }

    fn ticks(&self, now: u64) -> u64 {
        now.saturating_sub(self.base) / self.div()
    }

    /// Number of 1→0 transitions within `ticks` ticks after `base`.
    fn wraps(&self, ticks: u64) -> u64 {
        if self.load == 0 || ticks < 1 + u64::from(self.load) {
            0
        } else {
            (ticks - 1 - u64::from(self.load)) / self.period() + 1
        }
    }

    fn value(&self, now: u64) -> u32 {
        if !self.enabled() {
            return self.frozen;
        }
        let t = self.ticks(now);
        if t == 0 || self.load == 0 {
            0
        } else {
            self.load - ((t - 1) % self.period()) as u32
        }
    }

    /// Brings the model up to `now`. Returns true if the SysTick exception
    /// should be pended.
    pub fn update(&mut self, now: u64) -> bool {
        if !self.enabled() {
            return false;
        }
        let wraps = self.wraps(self.ticks(now));
        if wraps > self.wraps_seen {
            self.wraps_seen = wraps;
            self.csr |= CSR_COUNTFLAG;
            return self.csr & CSR_TICKINT != 0;
        }
        false
    }

    /// Cycle of the next counter wrap, if the timer runs.
    pub fn next_event(&self) -> Option<u64> {
        if !self.enabled() || self.load == 0 {
            return None;
        }
        let ticks = 1 + u64::from(self.load) + self.wraps_seen * self.period();
        Some(self.base + ticks * self.div())
    }

    fn restart_from(&mut self, now: u64, value: u32) {
        // Choose `base` so that the counter reads `value` at `now`.
        let ticks_back = if value == 0 || self.load == 0 {
            0
        } else {
            u64::from(self.load.saturating_sub(value)) + 1
        };
        self.base = now.saturating_sub(ticks_back * self.div());
        self.wraps_seen = self.wraps(self.ticks(now));
    }

    pub fn read(&mut self, offset: u32, now: u64) -> u32 {
        match offset {
            0x0 => {
                let v = self.csr;
                self.csr &= !CSR_COUNTFLAG;
                v
            }
            0x4 => self.load,
            0x8 => self.value(now),
            // CALIB: NOREF=0, SKEW=1, TENMS = 0 (unknown)
            0xC => 1 << 30,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, value: u32, now: u64) {
        match offset {
            0x0 => {
                let cur = self.value(now);
                let was_enabled = self.enabled();
                self.csr = (self.csr & CSR_COUNTFLAG) | (value & (CSR_ENABLE | CSR_TICKINT | CSR_CLKSOURCE));
                if self.enabled() {
                    if !was_enabled || self.div() != 1 {
                        self.restart_from(now, cur);
                    }
                } else {
                    self.frozen = cur;
                }
            }
            0x4 => {
                let cur = self.value(now);
                self.load = value & 0x00FF_FFFF;
                self.restart_from(now, cur.min(self.load));
            }
            0x8 => {
                // Any write clears the counter and COUNTFLAG.
                self.csr &= !CSR_COUNTFLAG;
                self.frozen = 0;
                self.restart_from(now, 0);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hal_style_1ms_tick() {
        let mut st = SysTick::default();
        st.write(0x4, 179_999, 0);
        st.write(0x8, 0, 0);
        st.write(0x0, 0b111, 0);
        assert_eq!(st.next_event(), Some(180_000));
        assert!(!st.update(179_999));
        assert!(st.update(180_000));
        assert_eq!(st.next_event(), Some(360_000));
        assert_eq!(st.read(0x0, 180_000) & CSR_COUNTFLAG, CSR_COUNTFLAG);
        assert_eq!(st.read(0x0, 180_000) & CSR_COUNTFLAG, 0);
        assert_eq!(st.read(0x8, 180_001), 179_999);
    }

    #[test]
    fn disabled_counter_freezes() {
        let mut st = SysTick::default();
        st.write(0x4, 99, 0);
        st.write(0x0, 0b101, 0);
        let v = st.read(0x8, 50);
        st.write(0x0, 0b100, 50);
        assert_eq!(st.read(0x8, 1000), v);
        assert_eq!(st.next_event(), None);
    }
}
