//! Cortex-M4F register set as GDB sees it.
//!
//! The order of the registers in the `g` packet is the order of the target
//! description below: r0–r12, sp, lr, pc, xpsr, msp, psp, primask, basepri,
//! faultmask, control, d0–d15, fpscr. GDB derives s0–s31 from d0–d15.

use std::num::NonZeroUsize;

use gdbstub::arch::{Arch, RegId, Registers};

pub enum CortexM4F {}

impl Arch for CortexM4F {
    type Usize = u32;
    type Registers = CortexMRegs;
    /// Thumb breakpoint kind (2, 3 or 4); breakpoints do not modify memory.
    type BreakpointKind = usize;
    type RegId = CortexMRegId;

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
  <architecture>arm</architecture>
  <feature name="org.gnu.gdb.arm.m-profile">
    <reg name="r0" bitsize="32" regnum="0" type="uint32" group="general"/>
    <reg name="r1" bitsize="32" type="uint32" group="general"/>
    <reg name="r2" bitsize="32" type="uint32" group="general"/>
    <reg name="r3" bitsize="32" type="uint32" group="general"/>
    <reg name="r4" bitsize="32" type="uint32" group="general"/>
    <reg name="r5" bitsize="32" type="uint32" group="general"/>
    <reg name="r6" bitsize="32" type="uint32" group="general"/>
    <reg name="r7" bitsize="32" type="uint32" group="general"/>
    <reg name="r8" bitsize="32" type="uint32" group="general"/>
    <reg name="r9" bitsize="32" type="uint32" group="general"/>
    <reg name="r10" bitsize="32" type="uint32" group="general"/>
    <reg name="r11" bitsize="32" type="uint32" group="general"/>
    <reg name="r12" bitsize="32" type="uint32" group="general"/>
    <reg name="sp" bitsize="32" type="data_ptr" group="general"/>
    <reg name="lr" bitsize="32" type="uint32" group="general"/>
    <reg name="pc" bitsize="32" type="code_ptr" group="general"/>
    <reg name="xpsr" bitsize="32" regnum="16" type="uint32" group="general"/>
  </feature>
  <feature name="org.gnu.gdb.arm.m-system">
    <reg name="msp" bitsize="32" regnum="17" type="data_ptr" group="system"/>
    <reg name="psp" bitsize="32" regnum="18" type="data_ptr" group="system"/>
  </feature>
  <feature name="its-emu.cortex-m.special">
    <reg name="primask" bitsize="32" regnum="19" type="uint32" group="system"/>
    <reg name="basepri" bitsize="32" regnum="20" type="uint32" group="system"/>
    <reg name="faultmask" bitsize="32" regnum="21" type="uint32" group="system"/>
    <reg name="control" bitsize="32" regnum="22" type="uint32" group="system"/>
  </feature>
  <feature name="org.gnu.gdb.arm.vfp">
    <reg name="d0" bitsize="64" regnum="23" type="ieee_double" group="float"/>
    <reg name="d1" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d2" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d3" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d4" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d5" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d6" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d7" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d8" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d9" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d10" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d11" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d12" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d13" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d14" bitsize="64" type="ieee_double" group="float"/>
    <reg name="d15" bitsize="64" type="ieee_double" group="float"/>
    <reg name="fpscr" bitsize="32" type="int" group="float"/>
  </feature>
</target>
"#;

/// Number of registers in the target description.
pub const NUM_REGS: usize = 40;
const FIRST_D: usize = 23;
const FPSCR: usize = 39;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CortexMRegs {
    /// r0–r12, sp, lr, pc, xpsr, msp, psp, primask, basepri, faultmask, control.
    pub core: [u32; 23],
    pub d: [u64; 16],
    pub fpscr: u32,
}

impl CortexMRegs {
    /// Size of register `n` in bytes.
    pub fn size(n: usize) -> Option<usize> {
        match n {
            0..FIRST_D | FPSCR => Some(4),
            FIRST_D..FPSCR => Some(8),
            _ => None,
        }
    }

    /// Little-endian bytes of register `n`.
    pub fn get(&self, n: usize) -> Option<Vec<u8>> {
        match n {
            0..FIRST_D => Some(self.core[n].to_le_bytes().to_vec()),
            FIRST_D..FPSCR => Some(self.d[n - FIRST_D].to_le_bytes().to_vec()),
            FPSCR => Some(self.fpscr.to_le_bytes().to_vec()),
            _ => None,
        }
    }

    /// Sets register `n` from little-endian bytes. Returns false if `n` or
    /// the length is wrong.
    pub fn set(&mut self, n: usize, bytes: &[u8]) -> bool {
        match (n, bytes.try_into(), bytes.try_into()) {
            (0..FIRST_D, Ok(b), _) => self.core[n] = u32::from_le_bytes(b),
            (FIRST_D..FPSCR, _, Ok(b)) => self.d[n - FIRST_D] = u64::from_le_bytes(b),
            (FPSCR, Ok(b), _) => self.fpscr = u32::from_le_bytes(b),
            _ => return false,
        }
        true
    }
}

impl Registers for CortexMRegs {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.core[15]
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for n in 0..NUM_REGS {
            for b in self.get(n).unwrap_or_default() {
                write_byte(Some(b));
            }
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let mut rest = bytes;
        for n in 0..NUM_REGS {
            let size = Self::size(n).ok_or(())?;
            if rest.len() < size {
                return Err(());
            }
            let (value, tail) = rest.split_at(size);
            if !self.set(n, value) {
                return Err(());
            }
            rest = tail;
        }
        Ok(())
    }
}

/// Register number in the target description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CortexMRegId(pub usize);

impl RegId for CortexMRegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        let size = CortexMRegs::size(id)?;
        Some((CortexMRegId(id), NonZeroUsize::new(size)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_matches_target_description() {
        // Every register in the XML in order, with its size.
        let sizes: Vec<usize> = TARGET_XML
            .lines()
            .filter_map(|l| l.split("bitsize=\"").nth(1))
            .map(|rest| rest.split('"').next().unwrap().parse::<usize>().unwrap() / 8)
            .collect();
        assert_eq!(sizes.len(), NUM_REGS);
        for (n, size) in sizes.iter().enumerate() {
            assert_eq!(CortexMRegs::size(n), Some(*size), "register {n}");
        }

        let mut regs = CortexMRegs::default();
        regs.core[15] = 0x0800_1234;
        regs.d[1] = 0x1122_3344_5566_7788;
        regs.fpscr = 0xAABB_CCDD;
        let mut bytes = Vec::new();
        regs.gdb_serialize(|b| bytes.push(b.unwrap()));
        assert_eq!(bytes.len(), sizes.iter().sum::<usize>());
        assert_eq!(&bytes[60..64], &[0x34, 0x12, 0x00, 0x08]);
        let mut back = CortexMRegs::default();
        back.gdb_deserialize(&bytes).unwrap();
        assert_eq!(back, regs);
    }
}
