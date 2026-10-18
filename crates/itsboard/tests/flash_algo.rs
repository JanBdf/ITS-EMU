//! Flash programming with the pack's flash algorithm (STM32F4xx_2048.FLM),
//! run on the emulated core the way pyOCD and Keil do: code in RAM, LR at a
//! BKPT, halting debug catches the return. Skipped without the CMSIS pack.

use emu_core::{CoreReg, ElfImage, StopReason};
use itsboard::Simulator;

const DHCSR: u32 = 0xE000_EDF0;
/// Algorithm base: a BKPT for the return, then PrgCode + PrgData.
const BASE: u32 = 0x2000_0000;
const CODE: u32 = BASE + 0x20;
const STACK_TOP: u32 = 0x2000_4000;
const BUFFER: u32 = 0x2000_8000;

fn flm() -> Option<Vec<u8>> {
    let mut roots = Vec::new();
    if let Ok(r) = std::env::var("CMSIS_PACK_ROOT") {
        roots.push(std::path::PathBuf::from(r));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::Path::new(&home).join(".cache/arm/packs"));
    }
    for root in roots {
        let dir = root.join("Keil/STM32F4xx_DFP");
        let Ok(versions) = std::fs::read_dir(&dir) else { continue };
        for v in versions.flatten() {
            if let Ok(data) = std::fs::read(v.path().join("CMSIS/Flash/STM32F4xx_2048.FLM")) {
                return Some(data);
            }
        }
    }
    eprintln!("skipping: STM32F4xx_DFP pack (STM32F4xx_2048.FLM) not installed");
    None
}

struct Algo {
    sim: Simulator,
    image: ElfImage,
    /// Address of PrgData (static base, R9).
    data: u32,
}

impl Algo {
    fn load(flm: &[u8]) -> Self {
        let image = ElfImage::parse(flm).unwrap();
        let mut sim = Simulator::blank(None).unwrap();
        let m = &mut sim.machine;
        // a debugger halts the core and loads the algorithm
        m.debug_write(DHCSR, 4, 0xA05F_0003).unwrap();
        m.debug_write(BASE, 4, 0xE00A_BE00).unwrap(); // BKPT #0
        let mut data = 0;
        for seg in image.segments.iter().filter(|s| s.addr < 0x2000) {
            m.debug_write_block(CODE + seg.addr, &seg.data, 4).unwrap();
            data = data.max(CODE + seg.addr + seg.data.len() as u32);
        }
        let data = image.symbol("FlashDevice").map_or(data, |_| CODE + 0x160);
        Self { sim, image, data }
    }

    /// Calls `name(args)` and returns R0.
    fn call(&mut self, name: &str, args: &[u32]) -> u32 {
        let entry = CODE + (self.image.symbol(name).unwrap().addr & !1);
        let m = &mut self.sim.machine;
        for (i, &a) in args.iter().enumerate() {
            m.set_reg(CoreReg::R(i as u8), a);
        }
        m.set_reg(CoreReg::R(9), self.data);
        m.set_reg(CoreReg::Sp, STACK_TOP);
        m.set_reg(CoreReg::Lr, BASE | 1);
        m.set_reg(CoreReg::Xpsr, 1 << 24);
        m.set_reg(CoreReg::Pc, entry | 1);
        m.debug_write(DHCSR, 4, 0xA05F_0001).unwrap(); // resume
        let start = m.clock().time_ns();
        loop {
            match m.run(10_000_000) {
                StopReason::Budget => {}
                StopReason::Halted { pc } => {
                    assert_eq!(pc, BASE, "{name} returned to the BKPT");
                    break;
                }
                other => panic!("{name}: {other:?} at {:#x}", m.pc()),
            }
            assert!(m.clock().time_ns() - start < 20_000_000_000, "{name} does not return");
        }
        m.reg(CoreReg::R(0))
    }
}

#[test]
fn erase_and_program_with_the_pack_flash_algorithm() {
    let Some(flm) = flm() else { return };
    let mut a = Algo::load(&flm);
    let page: Vec<u8> = (0..1024u32).map(|i| (i * 7 + 3) as u8).collect();

    // flash is erased on a blank board; program something to erase first
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 2]), 0);
    a.sim.machine.debug_write_block(BUFFER, &page, 4).unwrap();
    assert_eq!(a.call("ProgramPage", &[0x0800_0400, 1024, BUFFER]), 0);
    assert_eq!(a.call("UnInit", &[2]), 0);
    let mut back = vec![0u8; 1024];
    assert!(a.sim.machine.read_mem(0x0800_0400, &mut back));
    assert_eq!(back, page);

    // sector 0 (16 KB): the algorithm erases with PSIZE x8, typically 400 ms
    let t0 = a.sim.time_ns();
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 1]), 0);
    assert_eq!(a.call("EraseSector", &[0x0800_0000]), 0);
    assert_eq!(a.call("UnInit", &[1]), 0);
    let ms = (a.sim.time_ns() - t0) / 1_000_000;
    assert!((400..420).contains(&ms), "erase took {ms} ms");
    assert!(a.sim.machine.read_mem(0x0800_0400, &mut back));
    assert!(back.iter().all(|&b| b == 0xFF), "erased");

    // a sector of bank 2 (sector 12 at 0x0810_0000)
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 2]), 0);
    assert_eq!(a.call("ProgramPage", &[0x0810_0000, 1024, BUFFER]), 0);
    assert_eq!(a.call("UnInit", &[2]), 0);
    assert!(a.sim.machine.read_mem(0x0810_0000, &mut back));
    assert_eq!(back, page);
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 1]), 0);
    assert_eq!(a.call("EraseSector", &[0x0810_0000]), 0);
    assert!(a.sim.machine.read_mem(0x0810_0000, &mut back));
    assert!(back.iter().all(|&b| b == 0xFF), "bank 2 sector erased");
}

#[test]
fn code_programmed_into_flash_runs() {
    // Translations of the old flash contents must not survive programming.
    let Some(flm) = flm() else { return };
    let mut a = Algo::load(&flm);
    // flash function at 0x0800_1000: MOVS r0, #42; BX lr
    let f1 = [0x2A, 0x20, 0x70, 0x47];
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 2]), 0);
    a.sim.machine.debug_write_block(BUFFER, &[f1.as_slice(), &[0xFF; 1020]].concat(), 4).unwrap();
    assert_eq!(a.call("ProgramPage", &[0x0800_1000, 1024, BUFFER]), 0);
    let call_flash = |a: &mut Algo| {
        let m = &mut a.sim.machine;
        m.set_reg(CoreReg::Sp, STACK_TOP);
        m.set_reg(CoreReg::Lr, BASE | 1);
        m.set_reg(CoreReg::Pc, 0x0800_1001);
        m.debug_write(DHCSR, 4, 0xA05F_0001).unwrap();
        assert!(matches!(m.run(1_000_000), StopReason::Halted { .. }));
        m.reg(CoreReg::R(0))
    };
    assert_eq!(call_flash(&mut a), 42);
    // erase + program a different function at the same place
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 1]), 0);
    assert_eq!(a.call("EraseSector", &[0x0800_0000]), 0);
    assert_eq!(a.call("Init", &[0x0800_0000, 8_000_000, 2]), 0);
    let f2 = [0x07, 0x20, 0x70, 0x47]; // MOVS r0, #7; BX lr
    a.sim.machine.debug_write_block(BUFFER, &[f2.as_slice(), &[0xFF; 1020]].concat(), 4).unwrap();
    assert_eq!(a.call("ProgramPage", &[0x0800_1000, 1024, BUFFER]), 0);
    assert_eq!(call_flash(&mut a), 7);
}
