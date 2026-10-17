//! Virtual time base.
//!
//! The emulator counts executed CPU cycles. Wall-clock-like time is derived
//! from the cycle counter using the *current* core clock frequency, which the
//! clock tree model (RCC) may change at runtime (e.g. HSI 16 MHz → PLL 180 MHz).

const PS_PER_S: u128 = 1_000_000_000_000;

#[derive(Debug, Clone)]
pub struct Clock {
    cycles: u64,
    cpu_hz: u32,
    /// Cycle counter value at the last frequency change.
    seg_cycles: u64,
    /// Virtual time in picoseconds at the last frequency change.
    seg_ps: u64,
}

impl Clock {
    pub fn new(cpu_hz: u32) -> Self {
        assert!(cpu_hz > 0);
        Self { cycles: 0, cpu_hz, seg_cycles: 0, seg_ps: 0 }
    }

    /// Executed CPU cycles since reset.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    pub fn cpu_hz(&self) -> u32 {
        self.cpu_hz
    }

    pub fn advance(&mut self, cycles: u64) {
        self.cycles += cycles;
    }

    /// Changes the core clock frequency from now on.
    pub fn set_cpu_hz(&mut self, hz: u32) {
        assert!(hz > 0);
        if hz == self.cpu_hz {
            return;
        }
        self.seg_ps = self.time_ps();
        self.seg_cycles = self.cycles;
        self.cpu_hz = hz;
    }

    /// Virtual time since reset in picoseconds.
    pub fn time_ps(&self) -> u64 {
        let delta = u128::from(self.cycles - self.seg_cycles);
        self.seg_ps + (delta * PS_PER_S / u128::from(self.cpu_hz)) as u64
    }

    /// First cycle (not before the last frequency change) at which
    /// [`Self::time_ps`] reaches `ps`, assuming the frequency stays the same.
    pub fn cycle_at_ps(&self, ps: u64) -> u64 {
        let dt = u128::from(ps.saturating_sub(self.seg_ps));
        let delta = (dt * u128::from(self.cpu_hz)).div_ceil(PS_PER_S);
        self.seg_cycles.saturating_add(u64::try_from(delta).unwrap_or(u64::MAX))
    }

    /// Virtual time (ps) at `cycle` (not before the last frequency change).
    pub fn ps_at_cycle(&self, cycle: u64) -> u64 {
        let delta = u128::from(cycle.saturating_sub(self.seg_cycles));
        self.seg_ps + (delta * PS_PER_S / u128::from(self.cpu_hz)) as u64
    }

    pub fn time_ns(&self) -> u64 {
        self.time_ps() / 1000
    }

    /// Number of CPU cycles that correspond to `ns` nanoseconds at the current frequency.
    pub fn ns_to_cycles(&self, ns: u64) -> u64 {
        (u128::from(ns) * u128::from(self.cpu_hz) / 1_000_000_000) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_follows_frequency_changes() {
        let mut c = Clock::new(16_000_000);
        c.advance(16_000_000); // 1 s at 16 MHz
        assert_eq!(c.time_ns(), 1_000_000_000);
        c.set_cpu_hz(180_000_000);
        c.advance(90_000_000); // 0.5 s at 180 MHz
        assert_eq!(c.time_ns(), 1_500_000_000);
        assert_eq!(c.cycles(), 106_000_000);
    }

    #[test]
    fn cycle_at_ps_inverts_time_ps() {
        let mut c = Clock::new(16_000_000);
        c.advance(1000);
        c.set_cpu_hz(180_000_000);
        for ps in [0, 62_500_000, 62_500_001, 62_505_555, 62_505_556, 72_500_000, 1_000_000_000_007] {
            let cycle = c.cycle_at_ps(ps);
            let mut probe = c.clone();
            if cycle > probe.cycles() {
                probe.advance(cycle - probe.cycles());
                assert!(probe.time_ps() >= ps, "{ps}");
                let mut before = c.clone();
                before.advance(cycle - 1 - c.cycles());
                assert!(before.time_ps() < ps, "{ps}");
            } else {
                assert!(probe.time_ps() >= ps, "{ps}");
            }
        }
    }
}
