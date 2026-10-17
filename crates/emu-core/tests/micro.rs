//! Runs the clang-built self-test firmware in `tests/fixtures/micro`.

use emu_core::{Bus, CoreReg, ElfImage, Machine, MachineConfig, MemoryMap, StopReason, WatchKind};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/../../tests/fixtures/micro/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e} (run tests/fixtures/micro/build.sh)"))
}

fn stm32f4_config() -> MachineConfig {
    MachineConfig {
        memory: MemoryMap {
            flash_base: 0x0800_0000,
            flash_size: 2 * 1024 * 1024,
            flash_alias_at_zero: true,
            ram: vec![(0x2000_0000, 192 * 1024), (0x1000_0000, 64 * 1024)],
            rom_zero: vec![(0x1FFF_0000, 0x8000), (0x1FFF_C000, 0x1000)],
            sram_bitband: Some((0x2000_0000, 0x2200_0000)),
            periph_base: 0x4000_0000,
            periph_size: 0x2000_0000,
        },
        reset_cpu_hz: 16_000_000,
        quantum: 100_000,
        masked_quantum: 1_000,
    }
}

fn read_f32(m: &Machine, addr: u32) -> f32 {
    f32::from_bits(m.read_u32(addr).unwrap())
}

#[test]
fn exceptions_fixture() {
    let image = ElfImage::parse(&fixture("exceptions.elf")).unwrap();
    let mut m = Machine::new(stm32f4_config(), Bus::new()).unwrap();
    m.load_elf(&image).unwrap();
    m.reset();

    let stop = m.run(10_000_000);
    assert!(matches!(stop, StopReason::Breakpoint { .. }), "unexpected stop: {stop:?} at pc {:#x}", m.pc());
    let bkpt_pc = m.pc();
    let main_bkpt = image.symbol("Reset_Handler").unwrap();
    assert_eq!(image.symbolize(bkpt_pc).map(|(s, _)| s.name.as_str()), Some("Reset_Handler"), "{main_bkpt:?}");

    let r = image.symbol("results").unwrap().addr;
    let word = |i: u32| m.read_u32(r + 4 * i).unwrap();
    assert_eq!(word(0), 0x1234_ABCD, "magic");
    assert!(word(1) >= 20, "SysTick ticks: {}", word(1));
    assert_eq!(word(2), 42, "SVC handler saw the stacked r0");
    assert_eq!(word(3), 1, "STIR-triggered IRQ5 ran once");
    assert_eq!(word(4), 0x3000_0000, "BFAR");
    assert_ne!(word(5) & (1 << 9), 0, "CFSR.PRECISERR");
    assert_eq!(word(6), 1, "returned from BusFault handler");
    let mut x = 1.0f32;
    for _ in 0..50_000 {
        x = x * 0.999 + 0.5;
    }
    assert_eq!(read_f32(&m, r + 28).to_bits(), x.to_bits(), "thread FP context preserved across interrupts");

    // isr_acc follows x = 1.5x + 0.25 once per tick.
    let mut expect = 0.0f32;
    for _ in 0..word(1) {
        expect = expect * 1.5 + 0.25;
    }
    assert_eq!(read_f32(&m, r + 32), expect, "FP state inside ISR");
    assert_eq!(word(9), 0x00C0_FFEE, ".data initialised from flash");
    assert_eq!(word(10), 1, "done");

    // The FP loop alone takes > 100k cycles, i.e. it was interrupted many times.
    assert!(word(1) > 100, "SysTick ticks: {}", word(1));
}

#[test]
#[ignore]
fn debug_probe() {
    let image = ElfImage::parse(&fixture("exceptions.elf")).unwrap();
    let mut m = Machine::new(stm32f4_config(), Bus::new()).unwrap();
    m.load_elf(&image).unwrap();
    m.reset();
    for _ in 0..5 {
        let stop = m.run(3000);
        let s = m.system();
        eprintln!(
            "stop={stop:?} pc={:#x} cycles={} systick_next={:?} pending_systick={} active_systick={} xpsr={:#x} primask={} control={:#x}",
            m.pc(),
            m.clock().cycles(),
            s.scs.systick.next_event(),
            s.nvic.is_pending(15),
            s.nvic.is_active(15),
            m.reg(emu_core::CoreReg::Xpsr),
            m.reg(emu_core::CoreReg::Primask),
            m.reg(emu_core::CoreReg::Control),
        );
    }
}

fn boot_exceptions() -> (Machine, ElfImage) {
    let image = ElfImage::parse(&fixture("exceptions.elf")).unwrap();
    let mut m = Machine::new(stm32f4_config(), Bus::new()).unwrap();
    m.load_elf(&image).unwrap();
    m.reset();
    (m, image)
}

#[test]
fn breakpoint_in_interrupt_handler() {
    let (mut m, image) = boot_exceptions();
    let results = image.symbol("results").unwrap().addr;
    let handler = image.symbol("SysTick_Handler").unwrap().addr & !1;
    // Run first, so that the handler is already translated when the breakpoint is set.
    assert_eq!(m.run(50_000), StopReason::Budget);
    m.add_breakpoint(handler);

    assert_eq!(m.run(10_000_000), StopReason::DebugBreakpoint { pc: handler });
    assert_eq!(m.pc(), handler);
    let ticks = m.read_u32(results + 4).unwrap();
    // Resuming executes the handler once and stops at its next invocation.
    assert_eq!(m.run(10_000_000), StopReason::DebugBreakpoint { pc: handler });
    assert_eq!(m.read_u32(results + 4).unwrap(), ticks + 1);

    m.remove_breakpoint(handler);
    assert!(matches!(m.run(10_000_000), StopReason::Breakpoint { .. }), "runs to the final BKPT");
    assert_eq!(m.read_u32(results + 40), Some(1), "done");
}

#[test]
fn single_step_executes_one_instruction() {
    let (mut m, image) = boot_exceptions();
    let entry = image.symbol("Reset_Handler").unwrap().addr & !1;
    assert_eq!(m.pc(), entry);
    let mut pcs = vec![m.pc()];
    for i in 0..20 {
        let cycles = m.clock().cycles();
        assert_eq!(m.step(), StopReason::Step);
        assert_eq!(m.clock().cycles(), cycles + 1, "step {i}");
        pcs.push(m.pc());
    }
    // Running to a breakpoint at the 21st PC takes the same path.
    let (mut r, _) = boot_exceptions();
    r.add_breakpoint(pcs[20]);
    assert_eq!(r.run(1_000_000), StopReason::DebugBreakpoint { pc: pcs[20] });
    // Stepping from a breakpoint executes the instruction there.
    assert_eq!(r.step(), StopReason::Step);
    assert_ne!(r.pc(), pcs[20]);
    assert_eq!(r.breakpoints().collect::<Vec<_>>(), vec![pcs[20]]);
    // Normal execution works again afterwards (Unicorn's count hook is dropped).
    r.remove_breakpoint(pcs[20]);
    assert!(matches!(r.run(10_000_000), StopReason::Breakpoint { .. }));
}

#[test]
fn write_watchpoint_stops_after_the_store() {
    let (mut m, image) = boot_exceptions();
    let ticks = image.symbol("results").unwrap().addr + 4;
    let handler = image.symbol("SysTick_Handler").unwrap();
    assert_eq!(m.run(50_000), StopReason::Budget);
    assert!(m.add_watchpoint(ticks, 4, WatchKind::Write));
    for _ in 0..3 {
        let before = m.read_u32(ticks).unwrap();
        let stop = m.run(10_000_000);
        assert!(
            matches!(stop, StopReason::Watchpoint { addr, kind: WatchKind::Write, .. } if addr == ticks),
            "{stop:?}"
        );
        assert_eq!(m.read_u32(ticks).unwrap(), before + 1, "the store has completed");
        let pc = m.pc();
        assert!((handler.addr & !1..(handler.addr & !1) + handler.size).contains(&pc), "pc {pc:#x}");
    }
    // Reads do not trigger a write watchpoint; removing it lets the program finish.
    assert!(m.remove_watchpoint(ticks, 4, WatchKind::Write));
    assert!(matches!(m.run(10_000_000), StopReason::Breakpoint { .. }));
}

#[test]
fn breakpoint_at_current_pc_does_not_block_run() {
    let (mut m, _) = boot_exceptions();
    let pc = m.pc();
    m.add_breakpoint(pc);
    assert_eq!(m.run(1_000), StopReason::Budget);
    assert!(m.clock().cycles() >= 1_000);
}

/// Boots hand-assembled Thumb code: vector table (SP 0x2000_1000, all fault
/// handlers at 0x0800_0060 = `BKPT #2`), `code` at the reset vector 0x0800_0040.
fn boot_thumb(code: &[u16], literals: &[u32]) -> Machine {
    let mut flash = vec![0u8; 0x80];
    let mut vectors = [0x0800_0061u32; 16];
    vectors[0] = 0x2000_1000;
    vectors[1] = 0x0800_0041;
    for (i, v) in vectors.iter().enumerate() {
        flash[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    let mut at = 0x40;
    for half in code {
        flash[at..at + 2].copy_from_slice(&half.to_le_bytes());
        at += 2;
    }
    for lit in literals {
        flash[at..at + 4].copy_from_slice(&lit.to_le_bytes());
        at += 4;
    }
    flash[0x60..0x62].copy_from_slice(&0xBE02u16.to_le_bytes());
    let mut m = Machine::new(stm32f4_config(), Bus::new()).unwrap();
    m.load_bin(0x0800_0000, &flash).unwrap();
    m.reset();
    m
}

#[test]
fn code_runs_from_ram_and_may_modify_itself() {
    let mut m = boot_thumb(
        &[
            0x4803, // ldr r0, =0x2000_0101
            0x4780, // blx r0           (r1 = 42)
            0x1C0C, // adds r4, r1, #0
            0x4A03, // ldr r2, =0x2000_0100
            0x4B03, // ldr r3, =0x2107  (movs r1, #7)
            0x8013, // strh r3, [r2]    (patches the code in RAM)
            0x4780, // blx r0           (r1 = 7)
            0xBE01, // bkpt #1
        ],
        &[0x2000_0101, 0x2000_0100, 0x2107],
    );
    // in RAM: movs r1, #42; bx lr
    assert!(m.write_mem(0x2000_0100, &[0x2A, 0x21, 0x70, 0x47]));
    let stop = m.run(10_000);
    assert_eq!(stop, StopReason::Breakpoint { pc: 0x0800_004E });
    assert_eq!(m.reg(CoreReg::R(4)), 42, "code in RAM ran");
    assert_eq!(m.reg(CoreReg::R(1)), 7, "the modified code ran");
}

#[test]
fn fetch_from_peripheral_space_faults() {
    let mut m = boot_thumb(
        &[
            0x4801, // ldr r0, =0x4000_0001
            0x4700, // bx r0
            0xBF00, 0xBF00,
        ],
        &[0x4000_0001],
    );
    let stop = m.run(10_000);
    assert_eq!(stop, StopReason::Breakpoint { pc: 0x0800_0060 }, "fault handler");
    let scs = &m.system().scs;
    assert_ne!(scs.cfsr & (1 << 9), 0, "CFSR.PRECISERR {:#x}", scs.cfsr);
    assert_ne!(scs.hfsr & (1 << 30), 0, "escalated to HardFault {:#x}", scs.hfsr);
    let sp = m.reg(CoreReg::Sp);
    assert_eq!(m.read_u32(sp + 24), Some(0x4000_0000), "stacked return address");
}

#[test]
fn single_step_into_code_in_ram() {
    let mut m = boot_thumb(
        &[
            0x4801, // ldr r0, =0x2000_0101
            0x4700, // bx r0
            0xBF00, 0xBF00,
        ],
        &[0x2000_0101],
    );
    // in RAM: movs r1, #42; bkpt #1
    assert!(m.write_mem(0x2000_0100, &[0x2A, 0x21, 0x01, 0xBE]));
    assert_eq!(m.step(), StopReason::Step);
    assert_eq!(m.step(), StopReason::Step);
    assert_eq!(m.pc(), 0x2000_0100);
    assert_eq!(m.step(), StopReason::Step);
    assert_eq!((m.pc(), m.reg(CoreReg::R(1))), (0x2000_0102, 42), "the first step in RAM executes an instruction");
}
