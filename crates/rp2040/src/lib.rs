//! RP2040 (Raspberry Pi Pico): memory map, peripherals and the board.
//!
//! Core 0 runs the firmware from flash as boot stage 2 leaves it (XIP set up,
//! vector table at 0x1000_0100); core 1 starts in the boot ROM, which is the
//! real one, and waits there for core 0 to launch it.

pub mod adc;
pub mod bootrom;
pub mod flash;
pub mod pico;
pub mod pins;
pub mod pio;
pub mod pwm;
pub mod regs;
pub mod shared;
pub mod sio;
pub mod sys;
pub mod testfw;
pub mod timer;
pub mod uart;
pub mod uf2;
pub mod usb;

use emu_core::{Bus, CpuModel, MachineConfig, MemoryMap, Peripheral};

pub use pico::{Pico, PicoError};
pub use pins::PinEdge;

pub use regs::Notices;
use regs::{Aliased, BLOCK_WINDOW, Block, RegFile};

pub const ROM_BASE: u32 = 0x0000_0000;
pub const FLASH_BASE: u32 = 0x1000_0000;
/// W25Q16JV on the Pico.
pub const FLASH_SIZE: u32 = 2 * 1024 * 1024;
pub const SRAM_BASE: u32 = 0x2000_0000;
pub const SRAM_SIZE: u32 = 264 * 1024;
pub const XIP_SRAM_BASE: u32 = 0x1500_0000;
/// Boot stage 2 hands over to the vector table after itself.
pub const VECTOR_TABLE: u32 = FLASH_BASE + 0x100;
pub const SIO_BASE: u32 = 0xD000_0000;

pub fn memory_map() -> MemoryMap {
    MemoryMap {
        flash_base: FLASH_BASE,
        flash_size: FLASH_SIZE,
        flash_alias_at_zero: false,
        // the boot ROM is loaded into a region of its own
        ram: vec![(ROM_BASE, bootrom::ROM_SIZE as u32), (XIP_SRAM_BASE, 0x4000), (SRAM_BASE, SRAM_SIZE)],
        rom_zero: Vec::new(),
        sram_bitband: None,
        periph_base: 0x4000_0000,
        periph_size: 0x0010_0000,
        extra_periph: vec![
            (0x1400_0000, BLOCK_WINDOW), // XIP_CTRL
            (0x1800_0000, BLOCK_WINDOW), // XIP_SSI
            (0x5000_0000, 0x0040_0000),  // AHB-lite: DMA, USB, PIO
            (SIO_BASE, 0x1000),          // SIO
        ],
        // XIP without allocation / cache, without cache, ...
        flash_aliases: vec![0x1100_0000, 0x1200_0000, 0x1300_0000],
    }
}

/// Configuration of core `core` (0 or 1). Core 1 starts in the boot ROM.
pub fn machine_config(core: usize) -> MachineConfig {
    MachineConfig {
        memory: memory_map(),
        reset_cpu_hz: sys::ROSC_HZ,
        quantum: 200_000,
        masked_quantum: 1_000,
        cpu: CpuModel::CortexM0Plus,
        vector_table: if core == 0 { VECTOR_TABLE } else { ROM_BASE },
    }
}

/// Handles to the MCU state the board needs.
#[derive(Clone)]
pub struct Rp2040 {
    pub pins: pins::SharedPins,
    pub serial: usb::SharedSerial,
    /// What the firmware uses that is not emulated.
    pub notices: Notices,
    /// Voltages at the ADC inputs, temperature at the sensor.
    pub analog: adc::SharedAnalog,
}

fn attach<B: Block + 'static>(bus: &mut Bus, base: u32, block: B) {
    bus.attach(base, BLOCK_WINDOW, Box::new(Aliased(block)));
}

/// Each core's bus: its SIO, and the other peripherals, which both share.
pub fn build() -> ([Bus; 2], Rp2040) {
    let mut bus = Bus::new();
    let pins = pins::SharedPins::default();
    let serial = usb::SharedSerial::default();
    let tree = sys::SharedClockTree::default();
    let notices = Notices::default();

    static SYSINFO_FIXED: [(u32, u32); 3] = [(0x0, 0x2000_2927), (0x4, 0x2), (0x40, 0x0000_7A4B)];
    static PSM_FIXED: [(u32, u32); 1] = [(0xC, 0x0001_FFFF)];
    static VREG_RESET: [(u32, u32); 2] = [(0x0, 0x10B1), (0x4, 0x91)];
    static TBMAN_FIXED: [(u32, u32); 1] = [(0x0, 0x1)];
    static XIP_RESET: [(u32, u32); 1] = [(0x0, 0x3)];
    static XIP_FIXED: [(u32, u32); 1] = [(0x8, 0x3)];
    static PADS_QSPI_RESET: [(u32, u32); 7] =
        [(0x0, 0), (0x4, 0x67), (0x8, 0x6B), (0xC, 0x6B), (0x10, 0x6B), (0x14, 0x6B), (0x18, 0x5B)];

    attach(&mut bus, 0x4000_0000, RegFile::new("SYSINFO", 0x44, &[], &SYSINFO_FIXED));
    attach(&mut bus, 0x4000_4000, RegFile::new("SYSCFG", 0x20, &[], &[]));
    attach(&mut bus, 0x4000_8000, sys::Clocks::new(tree.clone()));
    attach(&mut bus, 0x4000_C000, sys::Resets::new());
    attach(&mut bus, 0x4001_0000, RegFile::new("PSM", 0x10, &[], &PSM_FIXED));
    attach(&mut bus, 0x4001_4000, pins::IoBank0::new(pins.clone()));
    let (ssi, io_qspi) = flash::flash_interface(notices.clone());
    attach(&mut bus, 0x4001_8000, io_qspi);
    attach(&mut bus, 0x4001_C000, pins::PadsBank0::new(pins.clone()));
    attach(&mut bus, 0x4002_0000, RegFile::new("PADS_QSPI", 0x1C, &PADS_QSPI_RESET, &[]));
    attach(&mut bus, 0x4002_4000, sys::Oscillator::xosc());
    attach(&mut bus, 0x4002_8000, sys::Pll::new(tree.clone(), 0));
    attach(&mut bus, 0x4002_C000, sys::Pll::new(tree.clone(), 1));
    attach(&mut bus, 0x4003_0000, RegFile::new("BUSCTRL", 0x28, &[], &[]));
    attach(&mut bus, 0x4003_4000, uart::Uart::new("UART0"));
    attach(&mut bus, 0x4003_8000, uart::Uart::new("UART1"));
    attach(&mut bus, 0x4003_C000, RegFile::unemulated("SPI0", 0x28, notices.clone()));
    attach(&mut bus, 0x4004_0000, RegFile::unemulated("SPI1", 0x28, notices.clone()));
    attach(&mut bus, 0x4004_4000, RegFile::unemulated("I2C0", 0x100, notices.clone()));
    attach(&mut bus, 0x4004_8000, RegFile::unemulated("I2C1", 0x100, notices.clone()));
    let analog = adc::SharedAnalog::default();
    attach(&mut bus, 0x4004_C000, adc::Adc::new(pins.clone(), tree.clone(), analog.clone(), notices.clone()));
    attach(&mut bus, 0x4005_0000, pwm::Pwm::new(pins.clone(), notices.clone()));
    attach(&mut bus, 0x4005_4000, timer::Timer::new());
    attach(&mut bus, 0x4005_8000, sys::Watchdog::new());
    attach(&mut bus, 0x4005_C000, RegFile::unemulated("RTC", 0x30, notices.clone()));
    attach(&mut bus, 0x4006_0000, sys::Oscillator::rosc());
    attach(&mut bus, 0x4006_4000, RegFile::new("VREG_AND_CHIP_RESET", 0xC, &VREG_RESET, &[]));
    attach(&mut bus, 0x4006_C000, RegFile::new("TBMAN", 0x4, &[], &TBMAN_FIXED));
    attach(&mut bus, 0x1400_0000, RegFile::new("XIP_CTRL", 0x20, &XIP_RESET, &XIP_FIXED));
    attach(&mut bus, 0x1800_0000, ssi);
    attach(&mut bus, 0x5000_0000, RegFile::unemulated("DMA", 0xB00, notices.clone()));
    let usb = usb::usb(serial.clone());
    bus.attach(0x5010_0000, 0x1000, Box::new(usb::UsbDpram(usb.clone())));
    attach(&mut bus, 0x5011_0000, usb::UsbRegs(usb));
    attach(&mut bus, 0x5020_0000, pio::Pio::new(0, pins.clone()));
    attach(&mut bus, 0x5030_0000, pio::Pio::new(1, pins.clone()));

    let shared = shared::SharedBus::new(bus, sys::ROSC_HZ);
    let sio = sio::SharedSio::default();
    let map = memory_map();
    let windows: Vec<_> = std::iter::once((map.periph_base, map.periph_size))
        .chain(map.extra_periph)
        .filter(|&(base, _)| base != SIO_BASE)
        .collect();
    let core_bus = |core: usize| {
        let mut bus = Bus::new();
        for &(base, size) in &windows {
            bus.attach(base, size, Box::new(shared.window(core, base)));
        }
        bus.attach(SIO_BASE, 0x1000, Box::new(sio::Sio::new(core, sio.clone(), pins.clone())) as Box<dyn Peripheral>);
        bus.attach_device(Box::new(shared.devices(core)));
        bus
    };
    ([core_bus(0), core_bus(1)], Rp2040 { pins, serial, notices, analog })
}
