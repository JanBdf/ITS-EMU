//! Halting debug through the debug port, as a probe (ST-LINK) drives it:
//! DHCSR, DCRSR/DCRDR, DEMCR, FPB, DWT on the AHB-AP.

mod common;

use common::Sim;
use emu_core::StopReason;

const DHCSR: u32 = 0xE000_EDF0;
const DCRSR: u32 = 0xE000_EDF4;
const DCRDR: u32 = 0xE000_EDF8;
const DEMCR: u32 = 0xE000_EDFC;
const DFSR: u32 = 0xE000_ED30;
const AIRCR: u32 = 0xE000_ED0C;
const S_HALT: u32 = 1 << 17;
const S_RESET_ST: u32 = 1 << 25;

fn reg(sim: &mut Sim, sel: u32) -> u32 {
    sim.machine.debug_write(DCRSR, 4, sel).unwrap();
    sim.machine.debug_read(DCRDR, 4).unwrap()
}

fn dhcsr(sim: &mut Sim) -> u32 {
    sim.machine.debug_read(DHCSR, 4).unwrap()
}

/// Runs until `ms` of virtual time or until the core halts.
fn run_until_ms_or_halt(sim: &mut Sim, ms: u64) -> StopReason {
    let target = ms * 1_000_000;
    while sim.machine.clock().time_ns() < target {
        let budget = sim.machine.clock().ns_to_cycles(target - sim.machine.clock().time_ns()).max(1);
        match sim.machine.run(budget) {
            StopReason::Budget => {}
            other => return other,
        }
    }
    StopReason::Budget
}

#[test]
fn single_step_in_a_hot_polling_loop_ends() {
    // HAL_Delay's loop has run for seconds: its translated code is cached
    // (without Unicorn's instruction counting).
    let Some(mut sim) = Sim::boot("TestStdinoutUSART.axf") else { return };
    sim.run_until_ms(12_000);
    let delay = sim.image.symbol("HAL_Delay").unwrap();
    assert!(sim.image.symbolize(sim.machine.pc()).is_some_and(|(s, _)| s.name == delay.name));
    for _ in 0..10 {
        let before = sim.machine.pc();
        assert_eq!(sim.machine.step(), StopReason::Step);
        assert_ne!(sim.machine.pc(), before);
    }
}

#[test]
fn halt_step_resume_while_running() {
    let Some(mut sim) = Sim::boot("TestStdinoutUSART.axf") else { return };
    sim.run_until_ms(12_000);
    sim.machine.debug_write(DHCSR, 4, 0xA05F_0003).unwrap();
    assert!(sim.machine.is_halted());
    assert_ne!(dhcsr(&mut sim) & S_HALT, 0);
    let pc = reg(&mut sim, 15);
    assert_eq!(pc, sim.machine.pc() & !1);
    assert!(matches!(sim.machine.run(1000), StopReason::Halted { .. }));
    // single steps (C_MASKINTS as pyOCD does)
    sim.machine.debug_write(DHCSR, 4, 0xA05F_000B).unwrap();
    for _ in 0..20 {
        sim.machine.debug_write(DFSR, 4, 0x1F).unwrap();
        sim.machine.debug_write(DHCSR, 4, 0xA05F_000D).unwrap();
        assert_ne!(dhcsr(&mut sim) & S_HALT, 0, "halted again after the step");
        assert_eq!(sim.machine.debug_read(DFSR, 4).unwrap() & 1, 1, "DFSR.HALTED");
    }
    // resume: runs on
    sim.machine.debug_write(DHCSR, 4, 0xA05F_0001).unwrap();
    assert!(!sim.machine.is_halted());
    assert_eq!(run_until_ms_or_halt(&mut sim, 13_000), StopReason::Budget);
}

#[test]
fn reset_with_vector_catch_halts_at_reset_handler() {
    let Some(mut sim) = Sim::boot("TestStdinoutUSART.axf") else { return };
    sim.run_until_ms(100);
    sim.machine.debug_write(DHCSR, 4, 0xA05F_0003).unwrap();
    sim.machine.debug_write(DEMCR, 4, 1).unwrap();
    sim.machine.debug_write(AIRCR, 4, 0x05FA_0004).unwrap();
    let v = dhcsr(&mut sim);
    assert_ne!(v & S_RESET_ST, 0);
    assert_ne!(v & S_HALT, 0);
    assert_eq!(dhcsr(&mut sim) & S_RESET_ST, 0, "S_RESET_ST is cleared by reading");
    let reset_vector = sim.machine.read_u32(4).unwrap();
    assert_eq!(reg(&mut sim, 15), reset_vector & !1);
    assert_eq!(sim.machine.debug_read(DFSR, 4).unwrap() & 8, 8, "DFSR.VCATCH");
    // without vector catch the core runs after the reset
    sim.machine.debug_write(DEMCR, 4, 0).unwrap();
    sim.machine.debug_write(AIRCR, 4, 0x05FA_0004).unwrap();
    assert_eq!(dhcsr(&mut sim) & S_HALT, 0);
}

#[test]
fn fpb_breakpoint_halts_at_main() {
    let Some(mut sim) = Sim::boot("TestStdinoutUSART.axf") else { return };
    let main = sim.image.symbol("main").unwrap().addr & !1;
    sim.machine.debug_write(DHCSR, 4, 0xA05F_0003).unwrap();
    sim.machine.debug_write(0xE000_2000, 4, 3).unwrap(); // FP_CTRL: KEY | ENABLE
    let replace = if main & 2 == 0 { 1 << 30 } else { 2 << 30 };
    sim.machine.debug_write(0xE000_2008, 4, replace | (main & 0x1FFF_FFFC) | 1).unwrap();
    sim.machine.debug_write(DHCSR, 4, 0xA05F_0001).unwrap();
    assert_eq!(run_until_ms_or_halt(&mut sim, 1000), StopReason::Halted { pc: main });
    assert_eq!(sim.machine.debug_read(DFSR, 4).unwrap() & 2, 2, "DFSR.BKPT");
    // stepping away from the breakpoint and resuming works
    sim.machine.debug_write(0xE000_2008, 4, 0).unwrap();
    sim.machine.debug_write(DHCSR, 4, 0xA05F_0001).unwrap();
    assert_eq!(run_until_ms_or_halt(&mut sim, 1100), StopReason::Budget);
}
