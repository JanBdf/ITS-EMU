//! ARMv7-M exception entry and return (stacking, EXC_RETURN, mode switches).
//!
//! Written against [`CoreAccess`] so that it does not depend on the CPU backend.

use crate::nvic::{MaskRegs, Nvic};

/// Core registers needed for exception handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreReg {
    R(u8), // R0..R12
    Sp,
    Lr,
    Pc,
    /// Full xPSR (APSR | IPSR | EPSR).
    Xpsr,
    Msp,
    Psp,
    Control,
    Primask,
    Faultmask,
    Basepri,
    Fpscr,
    S(u8), // S0..S31
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccessError {
    #[error("bus error at {0:#010x}")]
    Bus(u32),
}

pub trait CoreAccess {
    fn reg(&self, r: CoreReg) -> u32;
    fn set_reg(&mut self, r: CoreReg, v: u32);
    fn read_u32(&mut self, addr: u32) -> Result<u32, AccessError>;
    fn write_u32(&mut self, addr: u32, v: u32) -> Result<(), AccessError>;

    fn masks(&self) -> MaskRegs {
        MaskRegs {
            primask: self.reg(CoreReg::Primask) & 1 != 0,
            faultmask: self.reg(CoreReg::Faultmask) & 1 != 0,
            basepri: self.reg(CoreReg::Basepri) as u8,
        }
    }
}

const XPSR_T: u32 = 1 << 24;
const XPSR_IPSR_MASK: u32 = 0x1FF;
/// Bits of xPSR restored on exception return (everything except the stack align flag).
const XPSR_RESTORE_MASK: u32 = 0xFF0F_FDFF | XPSR_T;
const CONTROL_SPSEL: u32 = 1 << 1;
const CONTROL_FPCA: u32 = 1 << 2;
const EXC_RETURN_THREAD: u32 = 1 << 3;
const EXC_RETURN_PSP: u32 = 1 << 2;
const EXC_RETURN_BASIC_FRAME: u32 = 1 << 4;

const BASIC_FRAME: u32 = 0x20;
const EXTENDED_FRAME: u32 = 0x68;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExceptionError {
    #[error("bus error while stacking/unstacking at {0:#010x}")]
    Stacking(u32),
    #[error("invalid EXC_RETURN value {0:#010x}")]
    InvalidExcReturn(u32),
}

impl From<AccessError> for ExceptionError {
    fn from(e: AccessError) -> Self {
        match e {
            AccessError::Bus(a) => ExceptionError::Stacking(a),
        }
    }
}

/// Performs exception entry for `exc`. `return_addr` is the address execution
/// resumes at after the handler returns. The caller marks the exception active.
pub fn enter<C: CoreAccess>(
    core: &mut C,
    nvic: &mut Nvic,
    exc: u32,
    return_addr: u32,
    vtor: u32,
    stkalign: bool,
) -> Result<(), ExceptionError> {
    let xpsr = core.reg(CoreReg::Xpsr);
    let control = core.reg(CoreReg::Control);
    let in_thread = xpsr & XPSR_IPSR_MASK == 0;
    let use_psp = in_thread && control & CONTROL_SPSEL != 0;
    let fpca = control & CONTROL_FPCA != 0;

    let sp = core.reg(CoreReg::Sp);
    let frame_size = if fpca { EXTENDED_FRAME } else { BASIC_FRAME };
    let mut frame = sp.wrapping_sub(frame_size);
    let mut stacked_xpsr = xpsr;
    if stkalign && frame & 4 != 0 {
        frame -= 4;
        stacked_xpsr |= 1 << 9;
    } else {
        stacked_xpsr &= !(1 << 9);
    }

    let basic = [
        core.reg(CoreReg::R(0)),
        core.reg(CoreReg::R(1)),
        core.reg(CoreReg::R(2)),
        core.reg(CoreReg::R(3)),
        core.reg(CoreReg::R(12)),
        core.reg(CoreReg::Lr),
        return_addr & !1,
        stacked_xpsr,
    ];
    for (i, v) in basic.iter().enumerate() {
        core.write_u32(frame + 4 * i as u32, *v)?;
    }
    if fpca {
        // Non-lazy FP stacking: S0..S15, FPSCR.
        for i in 0..16u8 {
            core.write_u32(frame + 0x20 + 4 * u32::from(i), core.reg(CoreReg::S(i)))?;
        }
        core.write_u32(frame + 0x60, core.reg(CoreReg::Fpscr))?;
    }

    let mut exc_return = 0xFFFF_FFE1;
    if !fpca {
        exc_return |= EXC_RETURN_BASIC_FRAME;
    }
    if in_thread {
        exc_return |= EXC_RETURN_THREAD;
    }
    if use_psp {
        exc_return |= EXC_RETURN_PSP;
    }

    // Update the stack pointer that was used for stacking, then switch to MSP
    // while still in thread mode (SPSEL writes are ignored in handler mode).
    core.set_reg(CoreReg::Sp, frame);
    core.set_reg(CoreReg::Control, control & !(CONTROL_SPSEL | CONTROL_FPCA));
    // Enter handler mode: IPSR = exc, clear IT bits, keep flags.
    core.set_reg(CoreReg::Xpsr, (xpsr & 0xF80F_0000) | XPSR_T | exc);
    core.set_reg(CoreReg::Lr, exc_return);

    let handler = core.read_u32(vtor.wrapping_add(4 * exc))?;
    core.set_reg(CoreReg::Pc, handler | 1);
    nvic.acknowledge(exc);
    Ok(())
}

/// Performs exception return for the EXC_RETURN value `exc_return`.
pub fn exit<C: CoreAccess>(core: &mut C, nvic: &mut Nvic, exc_return: u32) -> Result<(), ExceptionError> {
    let exc_return = exc_return | 1;
    if exc_return & 0xFFFF_FFE0 != 0xFFFF_FFE0 || !matches!(exc_return & 0xF, 0x1 | 0x9 | 0xD) {
        return Err(ExceptionError::InvalidExcReturn(exc_return));
    }
    let to_thread = exc_return & EXC_RETURN_THREAD != 0;
    let use_psp = exc_return & EXC_RETURN_PSP != 0;
    let fp_frame = exc_return & EXC_RETURN_BASIC_FRAME == 0;

    let current = core.reg(CoreReg::Xpsr) & XPSR_IPSR_MASK;
    nvic.deactivate(current);

    let frame = if use_psp { core.reg(CoreReg::Psp) } else { core.reg(CoreReg::Msp) };
    let mut vals = [0u32; 8];
    for (i, v) in vals.iter_mut().enumerate() {
        *v = core.read_u32(frame + 4 * i as u32)?;
    }
    let mut fp = [0u32; 17];
    if fp_frame {
        for (i, v) in fp.iter_mut().enumerate() {
            *v = core.read_u32(frame + 0x20 + 4 * i as u32)?;
        }
    }
    let stacked_xpsr = vals[7];
    let mut new_sp = frame + if fp_frame { EXTENDED_FRAME } else { BASIC_FRAME };
    if stacked_xpsr & (1 << 9) != 0 {
        new_sp += 4;
    }

    let mut ipsr = stacked_xpsr & XPSR_IPSR_MASK;
    if to_thread {
        ipsr = 0;
    }
    // Leave handler mode first, then select the thread stack pointer.
    core.set_reg(CoreReg::Xpsr, (stacked_xpsr & XPSR_RESTORE_MASK & !XPSR_IPSR_MASK) | XPSR_T | ipsr);
    let mut control = core.reg(CoreReg::Control) & !(CONTROL_SPSEL | CONTROL_FPCA);
    if to_thread && use_psp {
        control |= CONTROL_SPSEL;
    }
    if fp_frame {
        control |= CONTROL_FPCA;
    }
    core.set_reg(CoreReg::Control, control);
    core.set_reg(CoreReg::Sp, new_sp);

    for (i, r) in [0u8, 1, 2, 3, 12].iter().enumerate() {
        core.set_reg(CoreReg::R(*r), vals[i]);
    }
    core.set_reg(CoreReg::Lr, vals[5]);
    if fp_frame {
        for i in 0..16u8 {
            core.set_reg(CoreReg::S(i), fp[i as usize]);
        }
        core.set_reg(CoreReg::Fpscr, fp[16]);
    }
    core.set_reg(CoreReg::Pc, vals[6] | 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvic;
    use std::collections::HashMap;

    /// Simple software model of the core register file for testing.
    #[derive(Default)]
    struct FakeCore {
        r: [u32; 13],
        lr: u32,
        pc: u32,
        xpsr: u32,
        msp: u32,
        psp: u32,
        control: u32,
        s: [u32; 32],
        fpscr: u32,
        mem: HashMap<u32, u32>,
    }

    impl CoreAccess for FakeCore {
        fn reg(&self, r: CoreReg) -> u32 {
            let handler = self.xpsr & 0x1FF != 0;
            match r {
                CoreReg::R(n) => self.r[n as usize],
                CoreReg::Sp => {
                    if !handler && self.control & 2 != 0 { self.psp } else { self.msp }
                }
                CoreReg::Lr => self.lr,
                CoreReg::Pc => self.pc,
                CoreReg::Xpsr => self.xpsr,
                CoreReg::Msp => self.msp,
                CoreReg::Psp => self.psp,
                CoreReg::Control => self.control,
                CoreReg::Fpscr => self.fpscr,
                CoreReg::S(n) => self.s[n as usize],
                _ => 0,
            }
        }
        fn set_reg(&mut self, r: CoreReg, v: u32) {
            let handler = self.xpsr & 0x1FF != 0;
            match r {
                CoreReg::R(n) => self.r[n as usize] = v,
                CoreReg::Sp => {
                    if !handler && self.control & 2 != 0 { self.psp = v } else { self.msp = v }
                }
                CoreReg::Lr => self.lr = v,
                CoreReg::Pc => self.pc = v,
                CoreReg::Xpsr => self.xpsr = v,
                CoreReg::Msp => self.msp = v,
                CoreReg::Psp => self.psp = v,
                CoreReg::Control => {
                    if handler {
                        self.control = (self.control & 2) | (v & !2);
                    } else {
                        self.control = v;
                    }
                }
                CoreReg::Fpscr => self.fpscr = v,
                CoreReg::S(n) => self.s[n as usize] = v,
                _ => {}
            }
        }
        fn read_u32(&mut self, a: u32) -> Result<u32, AccessError> {
            Ok(*self.mem.get(&a).unwrap_or(&0))
        }
        fn write_u32(&mut self, a: u32, v: u32) -> Result<(), AccessError> {
            self.mem.insert(a, v);
            Ok(())
        }
    }

    fn core() -> FakeCore {
        let mut c = FakeCore { msp: 0x2000_1000, pc: 0x0800_0100, xpsr: XPSR_T | 0xF000_0000, lr: 0x0800_0055, ..Default::default() };
        for i in 0..13 {
            c.r[i] = 0x100 + i as u32;
        }
        c.mem.insert(0x0800_0000 + 4 * nvic::SYSTICK, 0x0800_0201);
        c
    }

    #[test]
    fn entry_and_exit_roundtrip_basic_frame() {
        let mut c = core();
        let mut n = Nvic::new();
        enter(&mut c, &mut n, nvic::SYSTICK, 0x0800_0100, 0x0800_0000, true).unwrap();
        assert_eq!(c.pc, 0x0800_0201);
        assert_eq!(c.lr, 0xFFFF_FFF9);
        assert_eq!(c.xpsr & 0x1FF, nvic::SYSTICK);
        assert_eq!(c.msp, 0x2000_1000 - 0x20);
        assert!(n.is_active(nvic::SYSTICK));

        // Handler clobbers caller-saved registers.
        c.r[0] = 0xDEAD;
        c.r[12] = 0xBEEF;
        let lr = c.lr;
        exit(&mut c, &mut n, lr).unwrap();
        assert_eq!(c.pc, 0x0800_0101);
        assert_eq!(c.r[0], 0x100);
        assert_eq!(c.r[12], 0x10C);
        assert_eq!(c.lr, 0x0800_0055);
        assert_eq!(c.msp, 0x2000_1000);
        assert_eq!(c.xpsr & 0x1FF, 0);
        assert_eq!(c.xpsr & 0xF000_0000, 0xF000_0000);
        assert!(!n.is_active(nvic::SYSTICK));
    }

    #[test]
    fn misaligned_stack_is_realigned_and_restored() {
        let mut c = core();
        c.msp = 0x2000_0FFC;
        let mut n = Nvic::new();
        enter(&mut c, &mut n, nvic::SYSTICK, 0x0800_0100, 0x0800_0000, true).unwrap();
        assert_eq!(c.msp, 0x2000_0FFC - 0x20 - 4);
        let lr = c.lr;
        exit(&mut c, &mut n, lr).unwrap();
        assert_eq!(c.msp, 0x2000_0FFC);
    }

    #[test]
    fn psp_thread_and_fp_frame() {
        let mut c = core();
        c.psp = 0x2000_0800;
        c.control = CONTROL_SPSEL | CONTROL_FPCA;
        c.s[3] = 0x4049_0FDB;
        let mut n = Nvic::new();
        enter(&mut c, &mut n, nvic::SYSTICK, 0x0800_0100, 0x0800_0000, true).unwrap();
        assert_eq!(c.lr, 0xFFFF_FFED);
        assert_eq!(c.psp, 0x2000_0800 - 0x68);
        assert_eq!(c.control & (CONTROL_SPSEL | CONTROL_FPCA), 0);
        c.s[3] = 0;
        let lr = c.lr;
        exit(&mut c, &mut n, lr).unwrap();
        assert_eq!(c.psp, 0x2000_0800);
        assert_eq!(c.s[3], 0x4049_0FDB);
        assert_eq!(c.control, CONTROL_SPSEL | CONTROL_FPCA);
    }
}
