//! Core of the ITS-Board emulator: a Cortex-M4F machine (CPU via Unicorn,
//! NVIC, SysTick, exception handling, virtual time) plus the peripheral bus that
//! MCU-specific crates plug their peripherals into.

pub mod bus;
pub mod clock;
pub mod elf;
pub mod exception;
#[cfg(feature = "gdb")]
pub mod gdb;
pub mod machine;
pub mod nvic;
pub mod scs;
pub mod systick;
pub mod timing;

pub use bus::{Bus, DeviceCtx, Peripheral, ResetKind};
pub use clock::Clock;
pub use elf::ElfImage;
pub use exception::CoreReg;
pub use machine::{Machine, MachineConfig, MachineError, MemoryMap, StopReason, WatchKind};
