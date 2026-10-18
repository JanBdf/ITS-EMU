//! Debug architecture of the Cortex-M4 as a debugger sees it through the
//! debug port: halting debug (DHCSR, DCRSR/DCRDR, DEMCR, DFSR), the flash
//! patch and breakpoint unit (FPB), the DWT comparators and the CoreSight
//! identification (ROM table, component ID registers).
//!
//! This module holds the register state; [`Machine`](crate::Machine) carries
//! out what writes request (halting, register transfers, breakpoints). The
//! state belongs to the debug power domain: a system reset (SYSRESETREQ, the
//! reset pin) leaves it alone, except for the halt state and the sticky flags.

use crate::bus::{extract_read, merge_write};
use crate::machine::WatchKind;

pub const DHCSR: u32 = 0xEDF0;
pub const DCRSR: u32 = 0xEDF4;
pub const DCRDR: u32 = 0xEDF8;
pub const DEMCR: u32 = 0xEDFC;
pub const DFSR: u32 = 0xED30;

const DBGKEY: u32 = 0xA05F;
pub const C_DEBUGEN: u32 = 1 << 0;
pub const C_HALT: u32 = 1 << 1;
pub const C_STEP: u32 = 1 << 2;
pub const C_MASKINTS: u32 = 1 << 3;
const C_SNAPSTALL: u32 = 1 << 5;
const S_REGRDY: u32 = 1 << 16;
const S_HALT: u32 = 1 << 17;
const S_LOCKUP: u32 = 1 << 19;
const S_RETIRE_ST: u32 = 1 << 24;
const S_RESET_ST: u32 = 1 << 25;

pub const DFSR_HALTED: u32 = 1 << 0;
pub const DFSR_BKPT: u32 = 1 << 1;
pub const DFSR_DWTTRAP: u32 = 1 << 2;
pub const DFSR_VCATCH: u32 = 1 << 3;
pub const DFSR_EXTERNAL: u32 = 1 << 4;

pub const DEMCR_VC_CORERESET: u32 = 1 << 0;
pub const DEMCR_VC_MMERR: u32 = 1 << 4;
pub const DEMCR_VC_NOCPERR: u32 = 1 << 5;
pub const DEMCR_VC_CHKERR: u32 = 1 << 6;
pub const DEMCR_VC_STATERR: u32 = 1 << 7;
pub const DEMCR_VC_BUSERR: u32 = 1 << 8;
pub const DEMCR_VC_INTERR: u32 = 1 << 9;
pub const DEMCR_VC_HARDERR: u32 = 1 << 10;
const DEMCR_MASK: u32 = 0x010F_07F1;

const FPB_BASE: u32 = 0x2000;
const FP_NUM_CODE: usize = 6;
const FP_NUM_LIT: usize = 2;
const DWT_NUM_COMP: usize = 4;

/// Identification of the MCU vendor's parts of the debug system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugIds {
    /// Part number of the ROM table (ST uses the device ID).
    pub rom_table_part: u16,
    /// JEP106 manufacturer code of the ROM table: (continuation count, identity).
    pub rom_table_jep106: (u8, u8),
    /// Revision field of the ROM table's PIDR2.
    pub rom_table_revision: u8,
    /// DBGMCU block at 0xE004_2000 (STM32): its IDCODE register.
    pub dbgmcu_idcode: Option<u32>,
}

impl Default for DebugIds {
    /// A ROM table of Arm's own (no vendor block).
    fn default() -> Self {
        Self { rom_table_part: 0x4C4, rom_table_jep106: (4, 0x3B), rom_table_revision: 0, dbgmcu_idcode: None }
    }
}

/// What the machine has to do after a write to a debug register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugAction {
    None,
    /// DCRSR: transfer core register `sel` (to DCRDR, or from it if `write`).
    Register {
        sel: u32,
        write: bool,
    },
    /// DHCSR: leave debug state, executing one instruction if `step`.
    Resume {
        step: bool,
    },
    /// FPB or DWT comparators changed (or debugging was switched on/off).
    PointsChanged,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DwtComparator {
    comp: u32,
    mask: u32,
    function: u32,
    matched: bool,
}

#[derive(Debug, Clone)]
pub struct CoreDebug {
    ids: DebugIds,
    /// Control bits of DHCSR (C_*).
    control: u32,
    /// The core is in debug state (S_HALT).
    halted: bool,
    reset_st: bool,
    retire_st: bool,
    demcr: u32,
    dfsr: u32,
    dcrdr: u32,
    fp_enable: bool,
    fp_comp: [u32; FP_NUM_CODE + FP_NUM_LIT],
    dwt: [DwtComparator; DWT_NUM_COMP],
    dbgmcu: [u32; 3],
}

impl CoreDebug {
    pub fn new(ids: DebugIds) -> Self {
        Self {
            ids,
            control: 0,
            halted: false,
            reset_st: false,
            retire_st: false,
            demcr: 0,
            dfsr: 0,
            dcrdr: 0,
            fp_enable: false,
            fp_comp: [0; FP_NUM_CODE + FP_NUM_LIT],
            dwt: [DwtComparator::default(); DWT_NUM_COMP],
            dbgmcu: [0; 3],
        }
    }

    /// A debugger has enabled halting debug (DHCSR.C_DEBUGEN).
    pub fn enabled(&self) -> bool {
        self.control & C_DEBUGEN != 0
    }

    pub fn halted(&self) -> bool {
        self.halted
    }

    pub fn demcr(&self) -> u32 {
        self.demcr
    }

    /// DEMCR.TRCENA: DWT and ITM are enabled.
    pub fn trace_enabled(&self) -> bool {
        self.demcr & (1 << 24) != 0
    }

    pub fn dcrdr(&self) -> u32 {
        self.dcrdr
    }

    pub fn set_dcrdr(&mut self, v: u32) {
        self.dcrdr = v;
    }

    /// Enters debug state for the reasons in `dfsr` (DFSR bits).
    pub fn halt(&mut self, dfsr: u32) {
        self.halted = true;
        self.control |= C_HALT;
        self.dfsr |= dfsr;
    }

    /// The core executed instructions (S_RETIRE_ST).
    pub fn retired(&mut self) {
        self.retire_st = true;
    }

    /// System reset: the core leaves debug state unless reset vector catch
    /// is enabled. Returns true if it halts.
    pub fn on_reset(&mut self) -> bool {
        self.reset_st = true;
        self.retire_st = false;
        self.halted = false;
        self.control &= !(C_HALT | C_STEP);
        let catch = self.enabled() && self.demcr & DEMCR_VC_CORERESET != 0;
        if catch {
            self.halt(DFSR_VCATCH);
        }
        catch
    }

    /// Vector catch for fault exception `exc`, if enabled.
    pub fn catches_exception(&self, exc: u32) -> bool {
        if !self.enabled() {
            return false;
        }
        let bit = match exc {
            3 => DEMCR_VC_HARDERR,
            4 => DEMCR_VC_MMERR,
            5 => DEMCR_VC_BUSERR,
            6 => DEMCR_VC_STATERR | DEMCR_VC_CHKERR | DEMCR_VC_NOCPERR,
            _ => 0,
        };
        self.demcr & bit != 0
    }

    /// Whether this module implements PPB offset `off`.
    pub fn handles(&self, off: u32) -> bool {
        matches!(off & !3, DHCSR | DCRSR | DCRDR | DEMCR | DFSR)
            || (FPB_BASE..FPB_BASE + 0x1000).contains(&off)
            || (0x1020..0x1060).contains(&off)
            || (0xFF000..0x10_0000).contains(&off)
            || coresight_component(off).is_some()
            || (self.ids.dbgmcu_idcode.is_some() && (0x42000..0x42010).contains(&off))
    }

    /// Reads PPB offset `off` (for which [`handles`](Self::handles) is true).
    pub fn read(&mut self, off: u32, size: u32, lockup: bool) -> u32 {
        let aligned = off & !3;
        let word = match aligned {
            DHCSR => {
                let mut v = self.control | S_REGRDY;
                if self.halted {
                    v |= S_HALT;
                }
                if lockup {
                    v |= S_LOCKUP;
                }
                if self.retire_st {
                    v |= S_RETIRE_ST;
                }
                if self.reset_st {
                    v |= S_RESET_ST;
                }
                // sticky bits are cleared by the read
                self.retire_st = false;
                self.reset_st = false;
                v
            }
            DCRSR => 0,
            DCRDR => self.dcrdr,
            DEMCR => self.demcr,
            DFSR => self.dfsr,
            0x2000 => (FP_NUM_CODE as u32 & 0xF) << 4 | (FP_NUM_LIT as u32) << 8 | u32::from(self.fp_enable),
            0x2004 => 0, // FP_REMAP: no remapping
            0x2008..=0x2024 => self.fp_comp[((aligned - 0x2008) / 4) as usize],
            0x1020..=0x105C => {
                let n = ((aligned - 0x1020) / 16) as usize;
                let c = &mut self.dwt[n];
                match aligned & 0xC {
                    0x0 => c.comp,
                    0x4 => c.mask,
                    0x8 => {
                        let v = c.function | (u32::from(c.matched) << 24);
                        c.matched = false;
                        v
                    }
                    _ => 0,
                }
            }
            0x42000 => self.ids.dbgmcu_idcode.unwrap_or(0),
            0x42004..=0x4200C => self.dbgmcu[((aligned - 0x42004) / 4) as usize],
            0xFF000..=0xFFFFC => self.rom_table(aligned - 0xFF000),
            _ => coresight_component(aligned).map_or(0, |(c, reg)| c.id_register(reg)),
        };
        extract_read(word, off, size)
    }

    /// A write by the debugger (`debugger`) or by software running on the core.
    pub fn write(&mut self, off: u32, size: u32, value: u32, debugger: bool) -> DebugAction {
        let aligned = off & !3;
        let old = match aligned {
            DHCSR | DFSR | 0x1028 | 0x1038 | 0x1048 | 0x1058 => 0, // no read side effects
            _ => self.read(aligned, 4, false),
        };
        let v = merge_write(old, off, size, value);
        match aligned {
            DHCSR => {
                // Software cannot halt its own core; only the debugger writes DHCSR.
                if !debugger || size != 4 || v >> 16 != DBGKEY {
                    return DebugAction::None;
                }
                let was_enabled = self.enabled();
                let new = v & (C_DEBUGEN | C_HALT | C_STEP | C_MASKINTS | C_SNAPSTALL);
                self.control = new;
                if new & C_DEBUGEN == 0 {
                    // Debugging off: the core runs freely.
                    self.control = 0;
                    let was_halted = std::mem::take(&mut self.halted);
                    return if was_halted { DebugAction::Resume { step: false } } else { DebugAction::PointsChanged };
                }
                let action = if new & C_HALT != 0 {
                    if !self.halted {
                        self.halt(DFSR_HALTED);
                    }
                    DebugAction::None
                } else if self.halted {
                    self.halted = false;
                    DebugAction::Resume { step: new & C_STEP != 0 }
                } else {
                    DebugAction::None
                };
                if !was_enabled && action == DebugAction::None {
                    return DebugAction::PointsChanged;
                }
                return action;
            }
            DCRSR => {
                if debugger {
                    return DebugAction::Register { sel: v & 0x7F, write: v & (1 << 16) != 0 };
                }
            }
            DCRDR => self.dcrdr = v,
            DEMCR => self.demcr = v & DEMCR_MASK,
            DFSR => self.dfsr &= !v,
            0x2000 => {
                // KEY must be written as 1 for the write to take effect.
                if v & 2 != 0 && self.fp_enable != (v & 1 != 0) {
                    self.fp_enable = v & 1 != 0;
                    return DebugAction::PointsChanged;
                }
            }
            0x2008..=0x2024 => {
                let n = ((aligned - 0x2008) / 4) as usize;
                if self.fp_comp[n] != v {
                    self.fp_comp[n] = v & 0xDFFF_FFFD;
                    return DebugAction::PointsChanged;
                }
            }
            0x1020..=0x105C => {
                let n = ((aligned - 0x1020) / 16) as usize;
                let c = &mut self.dwt[n];
                match aligned & 0xC {
                    0x0 => c.comp = v,
                    0x4 => c.mask = v & 0x1F,
                    0x8 => c.function = v & 0x000F_0FAF,
                    _ => return DebugAction::None,
                }
                return DebugAction::PointsChanged;
            }
            0x42004..=0x4200C => self.dbgmcu[((aligned - 0x42004) / 4) as usize] = v,
            _ => {}
        }
        DebugAction::None
    }

    /// Instruction addresses at which the FPB halts the core.
    pub fn breakpoints(&self) -> Vec<u32> {
        if !self.enabled() || !self.fp_enable {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &c in &self.fp_comp[..FP_NUM_CODE] {
            if c & 1 == 0 {
                continue;
            }
            let base = c & 0x1FFF_FFFC;
            match c >> 30 {
                1 => out.push(base),
                2 => out.push(base + 2),
                3 => out.extend([base, base + 2]),
                _ => {} // remapping
            }
        }
        out
    }

    /// Data watchpoints (address, length, kind) and PC-match addresses of the DWT.
    pub fn dwt_points(&self) -> (Vec<(u32, u32, WatchKind)>, Vec<u32>) {
        let mut watch = Vec::new();
        let mut pcs = Vec::new();
        if !self.enabled() || !self.trace_enabled() {
            return (watch, pcs);
        }
        for c in &self.dwt {
            let len = 1u32 << c.mask.min(31);
            let addr = c.comp & !(len - 1);
            match c.function & 0xF {
                4 => pcs.push(addr),
                5 => watch.push((addr, len, WatchKind::Read)),
                6 => watch.push((addr, len, WatchKind::Write)),
                7 => watch.push((addr, len, WatchKind::Access)),
                _ => {}
            }
        }
        (watch, pcs)
    }

    /// A DWT comparator matched an access of `kind` at `addr`: sets MATCHED.
    /// Returns false if no DWT comparator covers it (a GDB watchpoint).
    pub fn dwt_matched(&mut self, addr: u32, kind: WatchKind) -> bool {
        if !self.enabled() || !self.trace_enabled() {
            return false;
        }
        let mut any = false;
        for c in &mut self.dwt {
            let len = 1u32 << c.mask.min(31);
            let k = match c.function & 0xF {
                5 => WatchKind::Read,
                6 => WatchKind::Write,
                7 => WatchKind::Access,
                _ => continue,
            };
            if c.comp & !(len - 1) == addr && k == kind {
                c.matched = true;
                any = true;
            }
        }
        any
    }

    /// ROM table at 0xE00F_F000 (offset `rel`).
    fn rom_table(&self, rel: u32) -> u32 {
        // Components of the Cortex-M4 relative to the table (two's complement offsets).
        const ENTRIES: [u32; 6] = [
            0xFFF0_F003, // SCS 0xE000_E000
            0xFFF0_2003, // DWT 0xE000_1000
            0xFFF0_3003, // FPB 0xE000_2000
            0xFFF0_1003, // ITM 0xE000_0000
            0xFFF4_1003, // TPIU 0xE004_0000
            0xFFF4_2003, // ETM 0xE004_1000
        ];
        if rel < 0xFCC {
            return ENTRIES.get((rel / 4) as usize).copied().unwrap_or(0);
        }
        let (cont, id) = self.ids.rom_table_jep106;
        let part = self.ids.rom_table_part;
        let pidr = [
            u32::from(cont),
            0,
            0,
            0,
            u32::from(part as u8),
            u32::from((part >> 8) as u8 & 0xF) | (u32::from(id & 0xF) << 4),
            u32::from((id >> 4) & 0x7) | 0x8 | (u32::from(self.ids.rom_table_revision) << 4),
            0,
        ];
        match rel {
            0xFCC => 1, // MEMTYPE: system memory present
            0xFD0..=0xFEC => pidr[((rel - 0xFD0) / 4) as usize],
            0xFF0..=0xFFC => [0x0D, 0x10, 0x05, 0xB1][((rel - 0xFF0) / 4) as usize],
            _ => 0,
        }
    }
}

/// An Arm component of the Cortex-M4 debug system.
struct Component {
    part: u16,
    revision: u8,
    class: u8,
    devtype: u8,
}

impl Component {
    /// Identification register at offset `reg` (0xFBC..0xFFC) of the component's 4 KB block.
    fn id_register(&self, reg: u32) -> u32 {
        let pidr = [
            0x04, // PIDR4: JEP106 continuation code 4 (Arm), 4 KB
            0,
            0,
            0,
            u32::from(self.part as u8),
            0xB0 | u32::from((self.part >> 8) as u8 & 0xF),
            0x0B | (u32::from(self.revision) << 4),
            0,
        ];
        match reg {
            0xFCC => u32::from(self.devtype),
            0xFD0..=0xFEC => pidr[((reg - 0xFD0) / 4) as usize],
            0xFF0..=0xFFC => [0x0D, u32::from(self.class) << 4, 0x05, 0xB1][((reg - 0xFF0) / 4) as usize],
            _ => 0,
        }
    }
}

/// The component whose identification registers are at PPB offset `off`.
fn coresight_component(off: u32) -> Option<(Component, u32)> {
    let reg = off & 0xFFF;
    if reg < 0xFCC {
        return None;
    }
    let c = match off & !0xFFF {
        0x0E000 => Component { part: 0x00C, revision: 0, class: 0xE, devtype: 0 }, // SCS
        0x01000 => Component { part: 0x002, revision: 3, class: 0xE, devtype: 0 }, // DWT
        0x02000 => Component { part: 0x003, revision: 2, class: 0xE, devtype: 0 }, // FPB
        0x00000 => Component { part: 0x001, revision: 3, class: 0xE, devtype: 0 }, // ITM
        0x40000 => Component { part: 0x9A1, revision: 0, class: 0x9, devtype: 0x11 }, // TPIU
        0x41000 => Component { part: 0x925, revision: 4, class: 0x9, devtype: 0x13 }, // ETM
        _ => return None,
    };
    Some((c, reg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st_ids() -> DebugIds {
        DebugIds { rom_table_part: 0x419, rom_table_jep106: (0, 0x20), rom_table_revision: 0, dbgmcu_idcode: None }
    }

    #[test]
    fn rom_table_identifies_st_device() {
        let mut d = CoreDebug::new(st_ids());
        // CheckID sequence of the STM32F4 pack
        let pidr1 = d.read(0xFFFE4, 4, false);
        let pidr2 = d.read(0xFFFE8, 4, false);
        assert_eq!(((pidr2 & 7) << 4) | ((pidr1 >> 4) & 0xF), 0x20);
        assert_eq!(d.read(0xFF000, 4, false), 0xFFF0_F003);
        assert_eq!(d.read(0xFFFF4, 4, false), 0x10, "ROM table class");
        // SCS: class 14 (generic IP), part 0x00C
        assert_eq!(d.read(0xEFF4, 4, false), 0xE0);
        assert_eq!(d.read(0xEFE0, 4, false), 0x0C);
    }

    #[test]
    fn halt_resume_and_sticky_reset_flag() {
        let mut d = CoreDebug::new(st_ids());
        // enabling debugging activates the comparators
        assert_eq!(d.write(DHCSR, 4, 0xA05F_0003, true), DebugAction::PointsChanged);
        assert!(d.halted());
        assert_ne!(d.read(DHCSR, 4, false) & S_HALT, 0);
        assert_eq!(d.read(DFSR, 4, false), DFSR_HALTED);
        // wrong key: ignored
        assert_eq!(d.write(DHCSR, 4, 0x0000_0001, true), DebugAction::None);
        assert!(d.halted());
        assert_eq!(d.write(DHCSR, 4, 0xA05F_000D, true), DebugAction::Resume { step: true });
        // reset with vector catch
        d.write(DEMCR, 4, DEMCR_VC_CORERESET, true);
        assert!(d.on_reset());
        let v = d.read(DHCSR, 4, false);
        assert_ne!(v & S_RESET_ST, 0);
        assert_eq!(d.read(DHCSR, 4, false) & S_RESET_ST, 0, "cleared by the read");
    }

    #[test]
    fn fpb_comparators_give_breakpoints() {
        let mut d = CoreDebug::new(st_ids());
        d.write(DHCSR, 4, 0xA05F_0001, true);
        d.write(0x2000, 4, 3, true);
        d.write(0x2008, 4, 0x4800_0101 | 1, true); // REPLACE=01: lower halfword of 0x0800_0100
        d.write(0x200C, 4, 0x8800_0204 | 1, true); // REPLACE=10: upper halfword of 0x0800_0204
        assert_eq!(d.breakpoints(), vec![0x0800_0100, 0x0800_0206]);
        d.write(0x2000, 4, 2, true);
        assert!(d.breakpoints().is_empty());
    }
}
