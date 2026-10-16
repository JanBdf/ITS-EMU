//! GDB remote debugging (feature `gdb`): the [`gdbstub`] target for
//! [`Machine`] and a TCP server ([`GdbServer`]) that hosts call between
//! emulation slices.

mod arch;
mod server;

pub use arch::{CortexM4F, CortexMRegs};
pub use server::{Execution, GdbServer};

use gdbstub::common::Signal;
use gdbstub::outputln;
use gdbstub::stub::SingleThreadStopReason;
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::base::single_register_access::{SingleRegisterAccess, SingleRegisterAccessOps};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep, SingleThreadSingleStepOps,
};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwBreakpoint, HwBreakpointOps, HwWatchpoint, HwWatchpointOps, SwBreakpoint,
    SwBreakpointOps, WatchKind as GdbWatchKind,
};
use gdbstub::target::ext::monitor_cmd::{ConsoleOutput, MonitorCmd, MonitorCmdOps};
use gdbstub::target::{Target, TargetError, TargetResult};

use crate::exception::CoreReg;
use crate::machine::{Machine, StopReason, WatchKind};
use arch::CortexMRegId;

/// What GDB asked the target to do when it last resumed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    Continue,
    Step,
}

/// Registers 0–22 of the target description.
const CORE_REGS: [CoreReg; 23] = [
    CoreReg::R(0),
    CoreReg::R(1),
    CoreReg::R(2),
    CoreReg::R(3),
    CoreReg::R(4),
    CoreReg::R(5),
    CoreReg::R(6),
    CoreReg::R(7),
    CoreReg::R(8),
    CoreReg::R(9),
    CoreReg::R(10),
    CoreReg::R(11),
    CoreReg::R(12),
    CoreReg::Sp,
    CoreReg::Lr,
    CoreReg::Pc,
    CoreReg::Xpsr,
    CoreReg::Msp,
    CoreReg::Psp,
    CoreReg::Primask,
    CoreReg::Basepri,
    CoreReg::Faultmask,
    CoreReg::Control,
];

impl Machine {
    fn gdb_regs(&self) -> CortexMRegs {
        let mut regs = CortexMRegs::default();
        for (v, r) in regs.core.iter_mut().zip(CORE_REGS) {
            *v = self.reg(r);
        }
        for (n, d) in regs.d.iter_mut().enumerate() {
            let lo = self.reg(CoreReg::S(2 * n as u8));
            let hi = self.reg(CoreReg::S(2 * n as u8 + 1));
            *d = u64::from(lo) | (u64::from(hi) << 32);
        }
        regs.fpscr = self.reg(CoreReg::Fpscr);
        regs
    }

    /// Writes the registers that differ from the current state. (GDB writes
    /// all registers at once; writing unchanged banked registers such as MSP
    /// after SP would undo a change of SP.)
    fn set_gdb_regs(&mut self, regs: &CortexMRegs) {
        let old = self.gdb_regs();
        for (i, r) in CORE_REGS.into_iter().enumerate() {
            if regs.core[i] != old.core[i] {
                // The core stays in Thumb state.
                let v = if r == CoreReg::Pc { regs.core[i] | 1 } else { regs.core[i] };
                self.set_reg(r, v);
            }
        }
        for n in 0..16 {
            if regs.d[n] != old.d[n] {
                self.set_reg(CoreReg::S(2 * n as u8), regs.d[n] as u32);
                self.set_reg(CoreReg::S(2 * n as u8 + 1), (regs.d[n] >> 32) as u32);
            }
        }
        if regs.fpscr != old.fpscr {
            self.set_reg(CoreReg::Fpscr, regs.fpscr);
        }
    }
}

/// GDB's view of why the target stopped.
fn stop_reason(reason: &StopReason) -> Option<SingleThreadStopReason<u32>> {
    Some(match reason {
        StopReason::Budget => return None,
        StopReason::Step => SingleThreadStopReason::DoneStep,
        StopReason::DebugBreakpoint { .. } => SingleThreadStopReason::SwBreak(()),
        StopReason::Breakpoint { .. } => SingleThreadStopReason::Signal(Signal::SIGTRAP),
        StopReason::Lockup { .. } => SingleThreadStopReason::Signal(Signal::SIGSEGV),
        &StopReason::Watchpoint { addr, kind, .. } => {
            SingleThreadStopReason::Watch { tid: (), kind: gdb_watch_kind(kind), addr }
        }
    })
}

fn gdb_watch_kind(kind: WatchKind) -> GdbWatchKind {
    match kind {
        WatchKind::Write => GdbWatchKind::Write,
        WatchKind::Read => GdbWatchKind::Read,
        WatchKind::Access => GdbWatchKind::ReadWrite,
    }
}

fn watch_kind(kind: GdbWatchKind) -> WatchKind {
    match kind {
        GdbWatchKind::Write => WatchKind::Write,
        GdbWatchKind::Read => WatchKind::Read,
        GdbWatchKind::ReadWrite => WatchKind::Access,
    }
}

impl Target for Machine {
    type Arch = CortexM4F;
    type Error = &'static str;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }

    // Breakpoints never modify memory, so GDB may use Z0 and Z1 freely.
    fn guard_rail_implicit_sw_breakpoints(&self) -> bool {
        true
    }
}

impl SingleThreadBase for Machine {
    fn read_registers(&mut self, regs: &mut CortexMRegs) -> TargetResult<(), Self> {
        *regs = self.gdb_regs();
        Ok(())
    }

    fn write_registers(&mut self, regs: &CortexMRegs) -> TargetResult<(), Self> {
        self.set_gdb_regs(regs);
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        if self.read_mem(start_addr, data) {
            return Ok(data.len());
        }
        // Partially mapped: return what is readable from the start.
        for (i, b) in data.iter_mut().enumerate() {
            let mut one = [0u8];
            if !self.read_mem(start_addr.wrapping_add(i as u32), &mut one) {
                return if i == 0 { Err(TargetError::NonFatal) } else { Ok(i) };
            }
            *b = one[0];
        }
        Ok(data.len())
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        if self.write_mem(start_addr, data) { Ok(()) } else { Err(TargetError::NonFatal) }
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<()> for Machine {
    fn read_register(&mut self, _tid: (), reg_id: CortexMRegId, buf: &mut [u8]) -> TargetResult<usize, Self> {
        let bytes = self.gdb_regs().get(reg_id.0).ok_or(TargetError::NonFatal)?;
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn write_register(&mut self, _tid: (), reg_id: CortexMRegId, val: &[u8]) -> TargetResult<(), Self> {
        let mut regs = self.gdb_regs();
        if !regs.set(reg_id.0, val) {
            return Err(TargetError::NonFatal);
        }
        self.set_gdb_regs(&regs);
        Ok(())
    }
}

impl SingleThreadResume for Machine {
    fn resume(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("signals cannot be delivered to a bare-metal target");
        }
        self.gdb_resume = Some(Resume::Continue);
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for Machine {
    fn step(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("signals cannot be delivered to a bare-metal target");
        }
        self.gdb_resume = Some(Resume::Step);
        Ok(())
    }
}

impl Breakpoints for Machine {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        Some(self)
    }
}

impl HwWatchpoint for Machine {
    fn add_hw_watchpoint(&mut self, addr: u32, len: u32, kind: GdbWatchKind) -> TargetResult<bool, Self> {
        Ok(self.add_watchpoint(addr, len, watch_kind(kind)))
    }

    fn remove_hw_watchpoint(&mut self, addr: u32, len: u32, kind: GdbWatchKind) -> TargetResult<bool, Self> {
        Ok(self.remove_watchpoint(addr, len, watch_kind(kind)))
    }
}

impl SwBreakpoint for Machine {
    fn add_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.add_breakpoint(addr);
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.remove_breakpoint(addr);
        Ok(true)
    }
}

impl HwBreakpoint for Machine {
    fn add_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.add_breakpoint(addr);
        Ok(true)
    }

    fn remove_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.remove_breakpoint(addr);
        Ok(true)
    }
}

impl MonitorCmd for Machine {
    fn handle_monitor_cmd(&mut self, cmd: &[u8], mut out: ConsoleOutput<'_>) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(cmd);
        let words: Vec<&str> = cmd.split_whitespace().collect();
        match words.as_slice() {
            // The target is halted whenever GDB can send a command, so "reset"
            // and "reset halt" are the same (as with pyOCD/OpenOCD + GDB).
            ["reset"] | ["reset", "halt" | "h" | "init"] => {
                self.reset();
                outputln!(out, "Resetting target");
            }
            ["halt"] => outputln!(out, "Target is halted"),
            ["help"] | [] => {
                outputln!(out, "monitor reset [halt]  reset the MCU, stay halted");
                outputln!(out, "monitor halt          (target is already halted)");
            }
            _ => outputln!(out, "unknown monitor command '{cmd}' (try 'monitor help')"),
        }
        Ok(())
    }
}
