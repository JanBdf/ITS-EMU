//! Shared harness for end-to-end tests with AC6-built programs.
#![allow(dead_code)]

use emu_core::{ElfImage, Machine, StopReason};
use itsboard::{ItsBoard, SCREEN_HEIGHT, SCREEN_WIDTH};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/ac6");
pub const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/golden");

pub struct Sim {
    pub machine: Machine,
    pub board: ItsBoard,
    pub image: ElfImage,
}

impl Sim {
    /// Boots a fixture, or `None` if it is not available (personal lab solutions).
    pub fn boot(name: &str) -> Option<Sim> {
        let Ok(data) = std::fs::read(format!("{FIXTURES}/{name}")) else {
            eprintln!("skipping: {name} not available (run tools/build-fixtures.sh)");
            return None;
        };
        let image = ElfImage::parse(&data).unwrap();
        let (bus, mcu) = stm32f429::build();
        let board = ItsBoard::attach(&mcu);
        let mut machine = stm32f429::machine(bus, &mcu).unwrap();
        machine.load_elf(&image).unwrap();
        machine.load_bin(0x0810_0000, &std::fs::read(format!("{FIXTURES}/fonts.bin")).unwrap()).unwrap();
        machine.reset();
        Some(Sim { machine, board, image })
    }

    pub fn run_until_ms(&mut self, ms: u64) {
        let target = ms * 1_000_000;
        while self.machine.clock().time_ns() < target {
            let budget = self.machine.clock().ns_to_cycles(target - self.machine.clock().time_ns()).max(1);
            assert_eq!(self.machine.run(budget), StopReason::Budget, "pc {:#x}", self.machine.pc());
        }
    }

    pub fn screen(&self) -> Vec<u8> {
        let mut rgba = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        self.board.render_display(&mut rgba);
        rgba
    }

    pub fn c_string(&self, symbol: &str, len: usize) -> String {
        let addr = self.image.symbol(symbol).unwrap().addr;
        let mut buf = vec![0u8; len];
        assert!(self.machine.read_mem(addr, &mut buf));
        String::from_utf8_lossy(&buf).trim_end_matches('\0').to_owned()
    }
}

impl Sim {
    /// Everything the program printed on the virtual COM port so far.
    pub fn take_serial(&self) -> String {
        String::from_utf8_lossy(&self.board.mcu().usart3.borrow_mut().take_output()).into_owned()
    }

    pub fn send_serial(&self, text: &str) {
        self.board.mcu().usart3.borrow_mut().send(text.as_bytes());
    }
}
