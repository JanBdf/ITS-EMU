//! Runs the clang-built self-test firmware in `tests/fixtures/micro`.

use emu_core::{Bus, ElfImage, Machine, MachineConfig, MemoryMap, StopReason};

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
