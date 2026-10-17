//! Exception state of an ARMv7-M core: enable/pending/active bits, priorities
//! and selection of the exception to take next.
//!
//! Exception numbers follow the architecture: 1 = Reset, 2 = NMI, 3 = HardFault,
//! ..., 15 = SysTick, 16 + n = external interrupt n.

pub const RESET: u32 = 1;
pub const NMI: u32 = 2;
pub const HARD_FAULT: u32 = 3;
pub const MEM_MANAGE: u32 = 4;
pub const BUS_FAULT: u32 = 5;
pub const USAGE_FAULT: u32 = 6;
pub const SVCALL: u32 = 11;
pub const DEBUG_MONITOR: u32 = 12;
pub const PENDSV: u32 = 14;
pub const SYSTICK: u32 = 15;
pub const IRQ0: u32 = 16;

/// Number of external interrupt lines supported by the model.
pub const NUM_IRQS: u32 = 96;
pub const NUM_EXCEPTIONS: usize = (IRQ0 + NUM_IRQS) as usize;

/// Number of implemented priority bits (STM32F4: 4).
pub const PRIO_BITS: u32 = 4;
const PRIO_MASK: u8 = (0xFFu32 << (8 - PRIO_BITS)) as u8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Bits([u32; NUM_EXCEPTIONS.div_ceil(32)]);

impl Bits {
    fn get(&self, n: u32) -> bool {
        self.0[(n / 32) as usize] & (1 << (n % 32)) != 0
    }
    fn set(&mut self, n: u32, v: bool) {
        let w = &mut self.0[(n / 32) as usize];
        if v {
            *w |= 1 << (n % 32);
        } else {
            *w &= !(1 << (n % 32));
        }
    }
    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.0
            .iter()
            .enumerate()
            .flat_map(|(i, &w)| (0..32).filter(move |b| w & (1 << b) != 0).map(move |b| i as u32 * 32 + b))
    }
    fn any(&self) -> bool {
        self.0.iter().any(|&w| w != 0)
    }
}

/// Masking state of the core that influences the execution priority.
#[derive(Debug, Clone, Copy, Default)]
pub struct MaskRegs {
    pub primask: bool,
    pub faultmask: bool,
    pub basepri: u8,
}

#[derive(Debug, Clone)]
pub struct Nvic {
    enabled: Bits,
    pending: Bits,
    active: Bits,
    /// Current level of external interrupt request lines (level-sensitive).
    level: Bits,
    /// Requests of the sources of lines that several peripherals share.
    sources: [u8; NUM_IRQS as usize],
    priority: [u8; NUM_EXCEPTIONS],
    prigroup: u8,
    /// SHCSR enable bits for MemManage/BusFault/UsageFault.
    pub mem_fault_ena: bool,
    pub bus_fault_ena: bool,
    pub usage_fault_ena: bool,
}

impl Default for Nvic {
    fn default() -> Self {
        Self::new()
    }
}

impl Nvic {
    pub fn new() -> Self {
        Self {
            enabled: Bits::default(),
            pending: Bits::default(),
            active: Bits::default(),
            level: Bits::default(),
            sources: [0; NUM_IRQS as usize],
            priority: [0; NUM_EXCEPTIONS],
            prigroup: 0,
            mem_fault_ena: false,
            bus_fault_ena: false,
            usage_fault_ena: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    // ----- register-level accessors -------------------------------------

    pub fn irq_enabled(&self, irq: u32) -> bool {
        self.enabled.get(IRQ0 + irq)
    }
    pub fn set_irq_enabled(&mut self, irq: u32, en: bool) {
        if irq < NUM_IRQS {
            self.enabled.set(IRQ0 + irq, en);
        }
    }
    pub fn is_pending(&self, exc: u32) -> bool {
        self.pending.get(exc)
    }
    pub fn set_pending(&mut self, exc: u32) {
        if (exc as usize) < NUM_EXCEPTIONS {
            self.pending.set(exc, true);
        }
    }
    pub fn clear_pending(&mut self, exc: u32) {
        if (exc as usize) < NUM_EXCEPTIONS {
            self.pending.set(exc, false);
        }
    }
    pub fn is_active(&self, exc: u32) -> bool {
        self.active.get(exc)
    }
    pub fn set_active(&mut self, exc: u32, a: bool) {
        self.active.set(exc, a);
    }

    /// Raw 8-bit priority field (only the upper `PRIO_BITS` are implemented).
    pub fn priority_field(&self, exc: u32) -> u8 {
        self.priority[exc as usize]
    }
    pub fn set_priority_field(&mut self, exc: u32, value: u8) {
        if exc >= MEM_MANAGE && (exc as usize) < NUM_EXCEPTIONS {
            self.priority[exc as usize] = value & PRIO_MASK;
        }
    }
    pub fn prigroup(&self) -> u8 {
        self.prigroup
    }
    pub fn set_prigroup(&mut self, g: u8) {
        self.prigroup = g & 7;
    }

    /// Sets the level of an external interrupt line. A high level pends the
    /// interrupt unless it is already pending or active; on deactivation a still
    /// asserted line pends it again.
    /// Sets the request of source `src` (0..8) of a shared interrupt line; the
    /// line is high while any of its sources requests.
    pub fn set_irq_source(&mut self, irq: u32, src: u8, high: bool) {
        let Some(s) = self.sources.get_mut(irq as usize) else { return };
        if high {
            *s |= 1 << src;
        } else {
            *s &= !(1 << src);
        }
        let level = *s != 0;
        self.set_irq_level(irq, level);
    }

    pub fn set_irq_level(&mut self, irq: u32, high: bool) {
        if irq >= NUM_IRQS {
            return;
        }
        let exc = IRQ0 + irq;
        self.level.set(exc, high);
        if high && !self.active.get(exc) {
            self.pending.set(exc, true);
        }
    }

    // ----- priority logic ------------------------------------------------

    fn group_mask(&self) -> i16 {
        ((0xFFu16 << (self.prigroup + 1)) & 0xFF) as i16
    }

    /// Priority of an exception as used for preemption (group priority).
    /// Fixed-priority exceptions are negative.
    pub fn group_priority(&self, exc: u32) -> i16 {
        match exc {
            RESET => -3,
            NMI => -2,
            HARD_FAULT => -1,
            _ => i16::from(self.priority[exc as usize]) & self.group_mask(),
        }
    }

    fn full_priority(&self, exc: u32) -> i16 {
        match exc {
            RESET => -3,
            NMI => -2,
            HARD_FAULT => -1,
            _ => i16::from(self.priority[exc as usize]),
        }
    }

    /// Execution priority without the boost from mask registers (256 = thread mode, nothing active).
    fn active_priority(&self) -> i16 {
        self.active.iter().map(|e| self.group_priority(e)).min().unwrap_or(256)
    }

    pub fn execution_priority(&self, masks: MaskRegs) -> i16 {
        let mut prio = self.active_priority();
        if masks.basepri & PRIO_MASK != 0 {
            prio = prio.min(i16::from(masks.basepri & PRIO_MASK) & self.group_mask());
        }
        if masks.primask {
            prio = prio.min(0);
        }
        if masks.faultmask {
            prio = prio.min(-1);
        }
        prio
    }

    fn is_enabled(&self, exc: u32) -> bool {
        match exc {
            MEM_MANAGE => self.mem_fault_ena,
            BUS_FAULT => self.bus_fault_ena,
            USAGE_FAULT => self.usage_fault_ena,
            e if e >= IRQ0 => self.enabled.get(e),
            _ => true,
        }
    }

    /// Highest priority pending and enabled exception, regardless of masking.
    pub fn highest_pending(&self) -> Option<u32> {
        self.pending.iter().filter(|&e| self.is_enabled(e)).min_by_key(|&e| (self.full_priority(e), e))
    }

    /// The exception that preempts the current execution, if any.
    pub fn pending_to_take(&self, masks: MaskRegs) -> Option<u32> {
        let exc = self.highest_pending()?;
        (self.group_priority(exc) < self.execution_priority(masks)).then_some(exc)
    }

    /// True if anything is pending (even if masked). Used to shorten time slices.
    pub fn any_pending(&self) -> bool {
        self.pending.any()
    }

    /// Number of active exceptions (RETTOBASE = active_count() <= 1).
    pub fn active_count(&self) -> usize {
        self.active.iter().count()
    }

    pub fn acknowledge(&mut self, exc: u32) {
        self.pending.set(exc, false);
        self.active.set(exc, true);
    }

    pub fn deactivate(&mut self, exc: u32) {
        self.active.set(exc, false);
        if self.level.get(exc) {
            self.pending.set(exc, true);
        }
    }

    /// Decides which exception is actually taken for a synchronous fault:
    /// a disabled or insufficiently prioritised configurable fault escalates to
    /// HardFault. Returns `None` on lockup (fault while at priority <= -1).
    pub fn route_fault(&self, exc: u32, masks: MaskRegs) -> Option<u32> {
        let exec = self.execution_priority(masks);
        let escalate = !self.is_enabled(exc) || self.group_priority(exc) >= exec;
        let target = if escalate { HARD_FAULT } else { exc };
        if target == HARD_FAULT && exec <= -1 {
            return None;
        }
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_highest_priority_and_respects_masks() {
        let mut n = Nvic::new();
        n.set_irq_enabled(3, true);
        n.set_irq_enabled(5, true);
        n.set_priority_field(IRQ0 + 3, 0x80);
        n.set_priority_field(IRQ0 + 5, 0x40);
        n.set_pending(IRQ0 + 3);
        n.set_pending(IRQ0 + 5);
        let m = MaskRegs::default();
        assert_eq!(n.pending_to_take(m), Some(IRQ0 + 5));
        assert_eq!(n.pending_to_take(MaskRegs { primask: true, ..m }), None);
        // BASEPRI 0x50 blocks priority 0x80 but not 0x40
        assert_eq!(n.pending_to_take(MaskRegs { basepri: 0x50, ..m }), Some(IRQ0 + 5));
        n.acknowledge(IRQ0 + 5);
        // IRQ3 (0x80) cannot preempt active IRQ5 (0x40)
        assert_eq!(n.pending_to_take(m), None);
        n.deactivate(IRQ0 + 5);
        assert_eq!(n.pending_to_take(m), Some(IRQ0 + 3));
    }

    #[test]
    fn disabled_fault_escalates() {
        let n = Nvic::new();
        assert_eq!(n.route_fault(USAGE_FAULT, MaskRegs::default()), Some(HARD_FAULT));
        let lock = MaskRegs { faultmask: true, ..Default::default() };
        assert_eq!(n.route_fault(USAGE_FAULT, lock), None);
    }

    #[test]
    fn level_interrupt_repends_after_return() {
        let mut n = Nvic::new();
        n.set_irq_enabled(0, true);
        n.set_irq_level(0, true);
        n.acknowledge(IRQ0);
        n.deactivate(IRQ0);
        assert!(n.is_pending(IRQ0));
    }
}
