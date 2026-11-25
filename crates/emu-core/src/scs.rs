//! Private peripheral bus of the Cortex-M4 (0xE000_0000 – 0xE00F_FFFF):
//! System Control Space (SCB, NVIC, SysTick, MPU, FPU registers) and DWT
//! (control, cycle counter). The debug registers (DHCSR, DEMCR, ...) are in
//! [`crate::debug`].

use crate::bus::{extract_read, merge_write};
use crate::nvic::{self, NUM_EXCEPTIONS, NUM_IRQS, Nvic};
use crate::systick::SysTick;

pub const PPB_BASE: u32 = 0xE000_0000;
pub const PPB_SIZE: u32 = 0x0010_0000;

const CPUID_CORTEX_M4_R0P1: u32 = 0x410F_C241;
/// ID_PFR0, ID_PFR1, ID_DFR0, ID_AFR0, ID_MMFR0..3, ID_ISAR0..4.
const ID_REGS: [u32; 13] = [
    0x0000_0030,
    0x0000_0200,
    0x0010_0000,
    0,
    0x0010_0030,
    0,
    0x0100_0000,
    0,
    0x0110_1110,
    0x0211_2000,
    0x2123_2231,
    0x0111_1131,
    0x0131_0132,
];
const AIRCR_VECTKEY: u32 = 0x05FA;

// CFSR bits
pub const CFSR_IBUSERR: u32 = 1 << 8;
pub const CFSR_PRECISERR: u32 = 1 << 9;
pub const CFSR_BFARVALID: u32 = 1 << 15;
pub const CFSR_UNDEFINSTR: u32 = 1 << 16;
pub const CFSR_INVSTATE: u32 = 1 << 17;
pub const CFSR_UNALIGNED: u32 = 1 << 24;
pub const CFSR_DIVBYZERO: u32 = 1 << 25;
pub const CFSR_IACCVIOL: u32 = 1 << 0;
pub const CFSR_DACCVIOL: u32 = 1 << 1;
pub const CFSR_MMARVALID: u32 = 1 << 7;
// HFSR bits
pub const HFSR_FORCED: u32 = 1 << 30;
pub const HFSR_DEBUGEVT: u32 = 1 << 31;

/// Access rights granted by the MPU for an address or page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpuPerms {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

/// Exception number whose priority byte lives at PPB offset `off`
/// (NVIC_IPRn or SCB_SHPRn), if any.
fn priority_byte_exception(off: u32) -> Option<u32> {
    if (0xE400..0xE400 + NUM_IRQS).contains(&off) {
        Some(nvic::IRQ0 + (off - 0xE400))
    } else if (0xED18..0xED24).contains(&off) {
        Some(nvic::MEM_MANAGE + (off - 0xED18))
    } else {
        None
    }
}

/// Side effects of an SCS write that the machine has to carry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScsAction {
    None,
    /// Something may have become pending; re-check exceptions soon.
    CheckPending,
    /// MPU configuration changed; flush the CPU TLB.
    FlushTlb,
    SystemReset,
}

#[derive(Debug, Clone)]
pub struct Scs {
    pub systick: SysTick,
    pub vtor: u32,
    pub scr: u32,
    pub ccr: u32,
    pub cfsr: u32,
    pub hfsr: u32,
    pub mmfar: u32,
    pub bfar: u32,
    pub cpacr: u32,
    pub mpu_ctrl: u32,
    mpu_rnr: u32,
    mpu_rbar: [u32; 8],
    mpu_rasr: [u32; 8],
    pub fpccr: u32,
    fpcar: u32,
    fpdscr: u32,
    dwt_ctrl: u32,
    /// CYCCNT = cycles - dwt_cyc_base
    dwt_cyc_base: u64,
}

impl Default for Scs {
    fn default() -> Self {
        Self::new()
    }
}

impl Scs {
    pub fn new() -> Self {
        Self {
            systick: SysTick::default(),
            vtor: 0,
            scr: 0,
            ccr: 0x200, // STKALIGN
            cfsr: 0,
            hfsr: 0,
            mmfar: 0,
            bfar: 0,
            cpacr: 0,
            mpu_ctrl: 0,
            mpu_rnr: 0,
            mpu_rbar: [0; 8],
            mpu_rasr: [0; 8],
            fpccr: 0xC000_0000, // ASPEN | LSPEN
            fpcar: 0,
            fpdscr: 0,
            dwt_ctrl: 0x4000_0000, // NUMCOMP = 4
            dwt_cyc_base: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// `cycles` passed while the core was halted (debug state): SysTick and
    /// DWT_CYCCNT count core cycles and stood still.
    pub fn core_halted_for(&mut self, cycles: u64) {
        self.systick.shift(cycles);
        self.dwt_cyc_base += cycles;
    }

    pub fn stkalign(&self) -> bool {
        self.ccr & 0x200 != 0
    }

    fn icsr(&self, nvic: &Nvic, ipsr: u32) -> u32 {
        let mut v = ipsr & 0x1FF;
        if nvic.active_count() <= 1 {
            v |= 1 << 11; // RETTOBASE
        }
        if let Some(p) = nvic.highest_pending() {
            v |= (p & 0x1FF) << 12;
        }
        if (nvic::IRQ0..NUM_EXCEPTIONS as u32).any(|e| nvic.is_pending(e)) {
            v |= 1 << 22; // ISRPENDING
        }
        if nvic.is_pending(nvic::SYSTICK) {
            v |= 1 << 26;
        }
        if nvic.is_pending(nvic::PENDSV) {
            v |= 1 << 28;
        }
        if nvic.is_pending(nvic::NMI) {
            v |= 1 << 31;
        }
        v
    }

    fn shcsr(&self, nvic: &Nvic) -> u32 {
        let mut v = 0;
        let bits = [
            (nvic::MEM_MANAGE, 0, true),
            (nvic::BUS_FAULT, 1, true),
            (nvic::USAGE_FAULT, 3, true),
            (nvic::SVCALL, 7, true),
            (nvic::DEBUG_MONITOR, 8, true),
            (nvic::PENDSV, 10, true),
            (nvic::SYSTICK, 11, true),
            (nvic::USAGE_FAULT, 12, false),
            (nvic::MEM_MANAGE, 13, false),
            (nvic::BUS_FAULT, 14, false),
            (nvic::SVCALL, 15, false),
        ];
        for (exc, bit, active) in bits {
            let set = if active { nvic.is_active(exc) } else { nvic.is_pending(exc) };
            if set {
                v |= 1 << bit;
            }
        }
        v |= u32::from(nvic.mem_fault_ena) << 16;
        v |= u32::from(nvic.bus_fault_ena) << 17;
        v |= u32::from(nvic.usage_fault_ena) << 18;
        v
    }

    fn write_shcsr(&mut self, nvic: &mut Nvic, v: u32) {
        nvic.mem_fault_ena = v & (1 << 16) != 0;
        nvic.bus_fault_ena = v & (1 << 17) != 0;
        nvic.usage_fault_ena = v & (1 << 18) != 0;
        let bits = [
            (nvic::MEM_MANAGE, 0, true),
            (nvic::BUS_FAULT, 1, true),
            (nvic::USAGE_FAULT, 3, true),
            (nvic::SVCALL, 7, true),
            (nvic::DEBUG_MONITOR, 8, true),
            (nvic::PENDSV, 10, true),
            (nvic::SYSTICK, 11, true),
            (nvic::USAGE_FAULT, 12, false),
            (nvic::MEM_MANAGE, 13, false),
            (nvic::BUS_FAULT, 14, false),
            (nvic::SVCALL, 15, false),
        ];
        for (exc, bit, active) in bits {
            let set = v & (1 << bit) != 0;
            if active {
                nvic.set_active(exc, set);
            } else if set {
                nvic.set_pending(exc);
            } else {
                nvic.clear_pending(exc);
            }
        }
    }

    /// See `Peripheral::read_stable_until`.
    pub fn read_stable_until(&self, off: u32, now: u64) -> u64 {
        match off & !3 {
            0x1004 => now,                                             // DWT_CYCCNT
            0xE010 if self.systick.countflag() => now,                 // cleared by the read
            0xE010 => self.systick.next_event().unwrap_or(u64::MAX),   // COUNTFLAG
            0xE018 => self.systick.next_tick(now).unwrap_or(u64::MAX), // VAL
            0xED04 => now,                                             // ICSR (pending state)
            _ => u64::MAX,
        }
    }

    /// Reads from PPB offset `off` (relative to 0xE000_0000).
    pub fn read(&mut self, nvic: &Nvic, now: u64, ipsr: u32, off: u32, size: u32) -> u32 {
        // Byte-addressable priority registers.
        if let Some(first) = priority_byte_exception(off) {
            let mut v = 0;
            for i in 0..size {
                let exc = first + i;
                if (exc as usize) < NUM_EXCEPTIONS {
                    v |= u32::from(nvic.priority_field(exc)) << (8 * i);
                }
            }
            return v;
        }
        let aligned = off & !3;
        let word = match aligned {
            // DWT
            0x1000 => self.dwt_ctrl,
            0x1004 => now.wrapping_sub(self.dwt_cyc_base) as u32,
            // SCS
            0xE004 => (NUM_IRQS / 32) - 1, // ICTR
            0xE010..=0xE01C => self.systick.read(aligned - 0xE010, now),
            0xE100..=0xE11C => self.irq_word(nvic, aligned - 0xE100, |n, e| n.irq_enabled(e - nvic::IRQ0)),
            0xE180..=0xE19C => self.irq_word(nvic, aligned - 0xE180, |n, e| n.irq_enabled(e - nvic::IRQ0)),
            0xE200..=0xE21C => self.irq_word(nvic, aligned - 0xE200, |n, e| n.is_pending(e)),
            0xE280..=0xE29C => self.irq_word(nvic, aligned - 0xE280, |n, e| n.is_pending(e)),
            0xE300..=0xE31C => self.irq_word(nvic, aligned - 0xE300, |n, e| n.is_active(e)),
            0xED00 => CPUID_CORTEX_M4_R0P1,
            // ID_PFR0..ID_ISAR4: processor features of the Cortex-M4 (read on the board)
            0xED40..=0xED70 => ID_REGS[((aligned - 0xED40) / 4) as usize],
            0xED04 => self.icsr(nvic, ipsr),
            0xED08 => self.vtor,
            0xED0C => (0xFA05 << 16) | (u32::from(nvic.prigroup()) << 8),
            0xED10 => self.scr,
            0xED14 => self.ccr,
            0xED24 => self.shcsr(nvic),
            0xED28 => self.cfsr,
            0xED2C => self.hfsr,
            0xED34 => self.mmfar,
            0xED38 => self.bfar,
            0xED88 => self.cpacr,
            0xED90 => 8 << 8, // MPU_TYPE: 8 data regions
            0xED94 => self.mpu_ctrl,
            0xED98 => self.mpu_rnr,
            0xED9C | 0xEDA4 | 0xEDAC | 0xEDB4 => self.mpu_rbar[(self.mpu_rnr & 7) as usize] | (self.mpu_rnr & 7),
            0xEDA0 | 0xEDA8 | 0xEDB0 | 0xEDB8 => self.mpu_rasr[(self.mpu_rnr & 7) as usize],
            0xEF34 => self.fpccr,
            0xEF38 => self.fpcar,
            0xEF3C => self.fpdscr,
            0xEF40 => 0x1011_0021, // MVFR0
            0xEF44 => 0x1100_0011, // MVFR1
            _ => 0,
        };
        extract_read(word, off, size)
    }

    fn irq_word(&self, nvic: &Nvic, rel: u32, f: impl Fn(&Nvic, u32) -> bool) -> u32 {
        let first = nvic::IRQ0 + (rel / 4) * 32;
        (0..32).filter(|i| first + i < NUM_EXCEPTIONS as u32 && f(nvic, first + i)).fold(0, |acc, i| acc | (1 << i))
    }

    /// Writes to PPB offset `off`.
    pub fn write(&mut self, nvic: &mut Nvic, now: u64, off: u32, size: u32, value: u32) -> ScsAction {
        if let Some(first) = priority_byte_exception(off) {
            for i in 0..size {
                nvic.set_priority_field(first + i, (value >> (8 * i)) as u8);
            }
            return ScsAction::CheckPending;
        }
        let aligned = off & !3;
        let old = self.read(nvic, now, 0, aligned, 4);
        let v = merge_write(old, off, size, value);
        match aligned {
            0x1000 => self.dwt_ctrl = (self.dwt_ctrl & 0xF000_0000) | (v & 0x0FFF_FFFF),
            0x1004 => self.dwt_cyc_base = now.wrapping_sub(u64::from(v)),
            0xE010..=0xE01C => {
                self.systick.write(aligned - 0xE010, v, now);
                return ScsAction::CheckPending;
            }
            0xE100..=0xE11C => self.irq_bits(value, aligned - 0xE100, |e| nvic.set_irq_enabled(e - nvic::IRQ0, true)),
            0xE180..=0xE19C => self.irq_bits(value, aligned - 0xE180, |e| nvic.set_irq_enabled(e - nvic::IRQ0, false)),
            0xE200..=0xE21C => self.irq_bits(value, aligned - 0xE200, |e| nvic.set_pending(e)),
            0xE280..=0xE29C => self.irq_bits(value, aligned - 0xE280, |e| nvic.clear_pending(e)),
            0xED04 => {
                if v & (1 << 31) != 0 {
                    nvic.set_pending(nvic::NMI);
                }
                if v & (1 << 28) != 0 {
                    nvic.set_pending(nvic::PENDSV);
                }
                if v & (1 << 27) != 0 {
                    nvic.clear_pending(nvic::PENDSV);
                }
                if v & (1 << 26) != 0 {
                    nvic.set_pending(nvic::SYSTICK);
                }
                if v & (1 << 25) != 0 {
                    nvic.clear_pending(nvic::SYSTICK);
                }
            }
            0xED08 => self.vtor = v & 0xFFFF_FF80,
            0xED0C => {
                if v >> 16 != AIRCR_VECTKEY {
                    return ScsAction::None;
                }
                nvic.set_prigroup(((v >> 8) & 7) as u8);
                if v & (1 << 2) != 0 {
                    return ScsAction::SystemReset;
                }
            }
            0xED10 => self.scr = v & 0x16,
            0xED14 => self.ccr = v & 0x31B,
            0xED24 => self.write_shcsr(nvic, v),
            0xED28 => self.cfsr &= !v,
            0xED2C => self.hfsr &= !v,
            0xED34 => self.mmfar = v,
            0xED38 => self.bfar = v,
            0xED88 => self.cpacr = v & (0xF << 20),
            0xED94 => {
                self.mpu_ctrl = v & 7;
                return ScsAction::FlushTlb;
            }
            0xED98 => self.mpu_rnr = v & 0xFF,
            0xED9C | 0xEDA4 | 0xEDAC | 0xEDB4 => {
                if v & 0x10 != 0 {
                    self.mpu_rnr = v & 7;
                }
                self.mpu_rbar[(self.mpu_rnr & 7) as usize] = v & !0x1F;
                return ScsAction::FlushTlb;
            }
            0xEDA0 | 0xEDA8 | 0xEDB0 | 0xEDB8 => {
                self.mpu_rasr[(self.mpu_rnr & 7) as usize] = v;
                return ScsAction::FlushTlb;
            }
            0xEF00 => {
                // STIR
                nvic.set_pending(nvic::IRQ0 + (v & 0x1FF));
            }
            0xEF34 => self.fpccr = v,
            0xEF38 => self.fpcar = v & !7,
            0xEF3C => self.fpdscr = v & 0x07C0_0000,
            _ => return ScsAction::None,
        }
        ScsAction::CheckPending
    }

    fn irq_bits(&self, value: u32, rel: u32, mut f: impl FnMut(u32)) {
        let first = nvic::IRQ0 + (rel / 4) * 32;
        for i in 0..32 {
            if value & (1 << i) != 0 && first + i < NUM_EXCEPTIONS as u32 {
                f(first + i);
            }
        }
    }

    /// True if `MPU_CTRL.ENABLE` is set.
    pub fn mpu_enabled(&self) -> bool {
        self.mpu_ctrl & 1 != 0
    }

    /// True if the MPU applies to accesses in the current execution state.
    pub fn mpu_active(&self, in_hfnmi: bool) -> bool {
        self.mpu_enabled() && (!in_hfnmi || self.mpu_ctrl & 2 != 0)
    }

    /// Access rights granted by the MPU for `addr`.
    ///
    /// Returns `None` when the default memory map applies (MPU inactive,
    /// System Control Space `0xE000_0000..=0xE00F_FFFF`, or privileged access
    /// falling through to the background region with `PRIVDEFENA`).
    pub fn mpu_access(&self, addr: u32, privileged: bool, in_hfnmi: bool) -> Option<MpuPerms> {
        if !self.mpu_active(in_hfnmi) || (0xE000_0000..=0xE00F_FFFF).contains(&addr) {
            return None;
        }
        let a = u64::from(addr);
        for i in (0..8).rev() {
            let rasr = self.mpu_rasr[i];
            if rasr & 1 == 0 {
                continue;
            }
            let size_field = (rasr >> 1) & 0x1F;
            if size_field < 4 {
                continue;
            }
            let size = 1u64 << (size_field + 1);
            let base = u64::from(self.mpu_rbar[i]) & !(size - 1);
            if a < base || a >= base + size {
                continue;
            }
            if size_field >= 7 {
                let srd = (rasr >> 8) & 0xFF;
                let sub = ((a - base) >> (size_field - 2)) & 7;
                if (srd >> sub) & 1 != 0 {
                    continue;
                }
            }
            let ap = (rasr >> 24) & 7;
            let (read, write) = match (ap, privileged) {
                (1, true) => (true, true),
                (2, true) => (true, true),
                (2, false) => (true, false),
                (3, _) => (true, true),
                (5, true) => (true, false),
                (6 | 7, _) => (true, false),
                _ => (false, false),
            };
            let exec = (rasr & (1 << 28)) == 0 && read;
            return Some(MpuPerms { read, write, exec });
        }
        if privileged && self.mpu_ctrl & 4 != 0 {
            None
        } else {
            Some(MpuPerms { read: false, write: false, exec: false })
        }
    }

    /// Effective permissions for the 1 KB TLB page starting at `page_base`
    /// (`page_base & 0x3FF == 0`), intersected across any sub-page MPU
    /// regions overlapping the page.
    pub fn mpu_page_perms(&self, page_base: u32, default: MpuPerms, privileged: bool, in_hfnmi: bool) -> MpuPerms {
        if !self.mpu_active(in_hfnmi) || (0xE000_0000..=0xE00F_FFFF).contains(&page_base) {
            return default;
        }
        let p_start = u64::from(page_base);
        let p_end = p_start + 1024;
        let mut subpage_overlap = false;
        for i in 0..8 {
            let rasr = self.mpu_rasr[i];
            if rasr & 1 == 0 {
                continue;
            }
            let size_field = (rasr >> 1) & 0x1F;
            if size_field < 4 {
                continue;
            }
            let size = 1u64 << (size_field + 1);
            let base = u64::from(self.mpu_rbar[i]) & !(size - 1);
            if base + size <= p_start || base >= p_end {
                continue;
            }
            let srd = (rasr >> 8) & 0xFF;
            if size < 1024 || (srd != 0 && (size / 8) < 1024) {
                subpage_overlap = true;
                break;
            }
        }
        if !subpage_overlap {
            return self.mpu_access(page_base, privileged, in_hfnmi).unwrap_or(default);
        }
        let mut perms = MpuPerms { read: true, write: true, exec: true };
        for off in (0..1024u32).step_by(32) {
            let p = self.mpu_access(page_base + off, privileged, in_hfnmi).unwrap_or(default);
            perms.read &= p.read;
            perms.write &= p.write;
            perms.exec &= p.exec;
        }
        perms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvic_enable_and_priority_registers() {
        let mut scs = Scs::new();
        let mut nvic = Nvic::new();
        // NVIC_EnableIRQ(TIM2_IRQn = 28)
        scs.write(&mut nvic, 0, 0xE100, 4, 1 << 28);
        assert!(nvic.irq_enabled(28));
        assert_eq!(scs.read(&nvic, 0, 0, 0xE100, 4), 1 << 28);
        scs.write(&mut nvic, 0, 0xE180, 4, 1 << 28);
        assert!(!nvic.irq_enabled(28));
        // IPR byte write keeps only 4 implemented bits
        scs.write(&mut nvic, 0, 0xE400 + 28, 1, 0x5F);
        assert_eq!(scs.read(&nvic, 0, 0, 0xE41C, 4), 0x50);
        // SHPR3: SysTick priority (byte 0xED23)
        scs.write(&mut nvic, 0, 0xED20, 4, 0xF000_0000);
        assert_eq!(nvic.priority_field(nvic::SYSTICK), 0xF0);
    }

    #[test]
    fn aircr_needs_key() {
        let mut scs = Scs::new();
        let mut nvic = Nvic::new();
        assert_eq!(scs.write(&mut nvic, 0, 0xED0C, 4, 0x0000_0304), ScsAction::None);
        assert_eq!(scs.write(&mut nvic, 0, 0xED0C, 4, 0x05FA_0300), ScsAction::CheckPending);
        assert_eq!(nvic.prigroup(), 3);
        assert_eq!(scs.write(&mut nvic, 0, 0xED0C, 4, 0x05FA_0004), ScsAction::SystemReset);
    }

    #[test]
    fn mpu_region_priority_subregions_and_privdefena() {
        let mut scs = Scs::new();
        let mut nvic = Nvic::new();
        // Region 0: 0x0000_0000, 32 B, no access
        assert_eq!(scs.write(&mut nvic, 0, 0xED98, 4, 0), ScsAction::CheckPending);
        assert_eq!(scs.write(&mut nvic, 0, 0xED9C, 4, 0x0000_0000), ScsAction::FlushTlb);
        assert_eq!(scs.write(&mut nvic, 0, 0xEDA0, 4, (4 << 1) | 1), ScsAction::FlushTlb);
        // Region 1: 0x2000_0000, 256 B, RO (AP=6), subregion 1 disabled (SRD bit 1)
        scs.write(&mut nvic, 0, 0xED9C, 4, 0x2000_0000 | 0x10 | 1);
        scs.write(&mut nvic, 0, 0xEDA0, 4, (6 << 24) | (0x02 << 8) | (7 << 1) | 1);
        // Enable MPU + PRIVDEFENA
        assert_eq!(scs.write(&mut nvic, 0, 0xED94, 4, 5), ScsAction::FlushTlb);

        let no_acc = Some(MpuPerms { read: false, write: false, exec: false });
        let ro = Some(MpuPerms { read: true, write: false, exec: true });
        assert_eq!(scs.mpu_access(0x0000_0000, true, false), no_acc);
        assert_eq!(scs.mpu_access(0x0000_001F, true, false), no_acc);
        assert_eq!(scs.mpu_access(0x0000_0020, true, false), None); // PRIVDEFENA background
        assert_eq!(scs.mpu_access(0x0000_0020, false, false), no_acc); // unprivileged has no background
        assert_eq!(scs.mpu_access(0x2000_0000, true, false), ro);
        assert_eq!(scs.mpu_access(0x2000_0020, true, false), None); // subregion 1 disabled -> background
        // Inactive in HardFault when HFNMIENA == 0
        assert_eq!(scs.mpu_access(0x0000_0000, true, true), None);
    }
}
