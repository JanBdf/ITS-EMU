//! A complete simulated ITS-Board: MCU, board devices and loaded firmware.

use emu_core::{ElfImage, Machine, MachineError, StopReason};

use crate::ItsBoard;

/// Font tables of ITS_BRD_LIB, normally flashed once to every board.
pub const DEFAULT_FONTS: &[u8] = include_bytes!("../assets/fonts.bin");
pub const FONTS_ADDR: u32 = 0x0810_0000;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Elf(#[from] emu_core::elf::ElfError),
    #[error(transparent)]
    Machine(#[from] MachineError),
}

pub struct Simulator {
    pub machine: Machine,
    pub board: ItsBoard,
    pub image: ElfImage,
}

impl Simulator {
    /// Creates a board, flashes `firmware` (+ `fonts`) and resets the MCU.
    pub fn new(firmware: &[u8], fonts: Option<&[u8]>) -> Result<Self, LoadError> {
        Self::with_image(ElfImage::parse(firmware)?, fonts)
    }

    /// A board with erased flash (+ `fonts`), waiting for a debugger to load
    /// a program. Like the real board, it locks up if run without one.
    pub fn blank(fonts: Option<&[u8]>) -> Result<Self, LoadError> {
        Self::with_image(ElfImage { entry: 0, segments: Vec::new(), symbols: Vec::new() }, fonts)
    }

    fn with_image(image: ElfImage, fonts: Option<&[u8]>) -> Result<Self, LoadError> {
        let (bus, mcu) = stm32f429::build();
        let board = ItsBoard::attach(&mcu);
        let mut machine = stm32f429::machine(bus, &mcu)?;
        machine.load_elf(&image)?;
        if let Some(fonts) = fonts {
            machine.load_bin(FONTS_ADDR, fonts)?;
        }
        machine.reset();
        Ok(Self { machine, board, image })
    }

    /// Virtual time since reset in nanoseconds.
    pub fn time_ns(&self) -> u64 {
        self.machine.clock().time_ns()
    }

    /// Runs for `ns` nanoseconds of virtual time, or until the core stops.
    pub fn run_for_ns(&mut self, ns: u64) -> StopReason {
        let target = self.time_ns().saturating_add(ns);
        while self.time_ns() < target {
            let budget = self.machine.clock().ns_to_cycles(target - self.time_ns()).max(1);
            match self.machine.run(budget) {
                StopReason::Budget => {}
                other => return other,
            }
        }
        StopReason::Budget
    }

    /// See [`ItsBoard::take_led_duty`].
    pub fn take_led_duty(&self) -> crate::LedDuty {
        self.board.take_led_duty(self.machine.clock().cycles())
    }

    /// Presses the reset button: the MCU restarts, flash content stays.
    pub fn reset(&mut self) {
        self.machine.reset();
    }

    /// `pc` as `0x0800_1234 <function+0x10>`.
    pub fn describe_pc(&self, pc: u32) -> String {
        match self.image.symbolize(pc) {
            Some((sym, off)) => format!("{pc:#010x} <{}+{off:#x}>", sym.name),
            None => format!("{pc:#010x}"),
        }
    }
}
