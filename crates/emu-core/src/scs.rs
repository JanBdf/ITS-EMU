//! Private peripheral bus of the Cortex-M4 (0xE000_0000 – 0xE00F_FFFF):
//! System Control Space (SCB, NVIC, SysTick, MPU, FPU registers) and DWT.

use crate::bus::{extract_read, merge_write};
use crate::nvic::{self, NUM_EXCEPTIONS, NUM_IRQS, Nvic};
use crate::systick::SysTick;

pub const PPB_BASE: u32 = 0xE000_0000;
pub const PPB_SIZE: u32 = 0x0010_0000;

const CPUID_CORTEX_M4_R0P1: u32 = 0x410F_C241;
const AIRCR_VECTKEY: u32 = 0x05FA;

// CFSR bits
pub const CFSR_IBUSERR: u32 = 1 << 8;
pub const CFSR_PRECISERR: u32 = 1 << 9;
pub const CFSR_BFARVALID: u32 = 1 << 15;
pub const CFSR_UNDEFINSTR: u32 = 1 << 16;
pub const CFSR_INVSTATE: u32 = 1 << 17;
pub const CFSR_UNALIGNED: u32 = 1 << 24;
pub const CFSR_DIVBYZERO: u32 = 1 << 25;
pub const CFSR_DACCVIOL: u32 = 1 << 1;
pub const CFSR_MMARVALID: u32 = 1 << 7;
// HFSR bits
pub const HFSR_FORCED: u32 = 1 << 30;

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
    demcr: u32,
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
            demcr: 0,
            dwt_ctrl: 0x4000_0000, // NUMCOMP = 4
            dwt_cyc_base: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn stkalign(&self) -> bool {
        self.ccr & 0x200 != 0
    }

    /// MPU region `n` as (RBAR, RASR).
    pub fn mpu_region(&self, n: usize) -> (u32, u32) {
        (self.mpu_rbar[n], self.mpu_rasr[n])
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
            0x1004 => now,                                              // DWT_CYCCNT
            0xE010 if self.systick.countflag() => now,                   // cleared by the read
            0xE010 => self.systick.next_event().unwrap_or(u64::MAX),    // COUNTFLAG
            0xE018 => self.systick.next_tick(now).unwrap_or(u64::MAX),  // VAL
            0xED04 => now,                                              // ICSR (pending state)
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
            0xEDFC => self.demcr,
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
        (0..32)
            .filter(|i| first + i < NUM_EXCEPTIONS as u32 && f(nvic, first + i))
            .fold(0, |acc, i| acc | (1 << i))
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
            0xED94 => self.mpu_ctrl = v & 7,
            0xED98 => self.mpu_rnr = v & 0xFF,
            0xED9C | 0xEDA4 | 0xEDAC | 0xEDB4 => {
                if v & 0x10 != 0 {
                    self.mpu_rnr = v & 7;
                }
                self.mpu_rbar[(self.mpu_rnr & 7) as usize] = v & !0x1F;
            }
            0xEDA0 | 0xEDA8 | 0xEDB0 | 0xEDB8 => self.mpu_rasr[(self.mpu_rnr & 7) as usize] = v,
            0xEDFC => self.demcr = v,
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
}
