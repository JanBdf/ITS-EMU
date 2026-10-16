//! STM32F429ZI: memory map and peripherals.

pub mod gpio;
pub mod rcc;
pub mod spi;
pub mod timer;

use emu_core::{Bus, MachineConfig, MemoryMap};

use gpio::{GPIOA_BASE, GpioPort, NUM_PORTS, PORT_STRIDE, SharedGpio};
use rcc::{FlashInterface, Pwr, Rcc, SharedClocks};
use spi::{Spi, SpiSlot};
use timer::{ApbBus, GpTimer, SharedPwm};

pub const FLASH_BASE: u32 = 0x0800_0000;
pub const FLASH_SIZE: u32 = 2 * 1024 * 1024;
pub const SRAM_BASE: u32 = 0x2000_0000;
/// SRAM1 + SRAM2 + SRAM3 (contiguous).
pub const SRAM_SIZE: u32 = 192 * 1024;
pub const CCM_BASE: u32 = 0x1000_0000;
pub const CCM_SIZE: u32 = 64 * 1024;
pub const PERIPH_BASE: u32 = 0x4000_0000;
pub const PERIPH_SIZE: u32 = 0x2000_0000;

/// Core clock after reset (HSI).
pub const HSI_HZ: u32 = 16_000_000;

pub fn memory_map() -> MemoryMap {
    MemoryMap {
        flash_base: FLASH_BASE,
        flash_size: FLASH_SIZE,
        flash_alias_at_zero: true,
        ram: vec![(SRAM_BASE, SRAM_SIZE), (CCM_BASE, CCM_SIZE)],
        // System memory + OTP, option bytes
        rom_zero: vec![(0x1FFF_0000, 0x8000), (0x1FFF_C000, 0x1000)],
        sram_bitband: Some((0x2000_0000, 0x2200_0000)),
        periph_base: PERIPH_BASE,
        periph_size: PERIPH_SIZE,
    }
}

pub fn machine_config() -> MachineConfig {
    MachineConfig { memory: memory_map(), reset_cpu_hz: HSI_HZ, quantum: 200_000, masked_quantum: 1_000 }
}

/// Handles to MCU state that board devices and front ends need.
#[derive(Clone)]
pub struct Stm32f429 {
    pub gpio: SharedGpio,
    pub clocks: SharedClocks,
    /// Device connected to SPI1 (set by the board model).
    pub spi1: SpiSlot,
    /// PWM outputs of TIM2..TIM5 (index 0 = TIM2).
    pub tim_pwm: [SharedPwm; 4],
}

/// Ethernet MAC/DMA registers: reads as idle, writes ignored. The LCD driver
/// briefly switches the MAC off around every SPI transfer, so the window must
/// exist (and stay quiet) even though Ethernet is not emulated.
struct EthStub;

impl emu_core::Peripheral for EthStub {
    fn name(&self) -> &str {
        "ETH"
    }

    fn read_depends_on_time(&self, _offset: u32) -> bool {
        false
    }
    fn read(&mut self, _: &mut emu_core::DeviceCtx, _: u32, _: u32) -> u32 {
        0
    }
    fn write(&mut self, _: &mut emu_core::DeviceCtx, _: u32, _: u32, _: u32) {}
}

/// Builds the peripheral bus with all modelled STM32F429 peripherals.
pub fn build() -> (Bus, Stm32f429) {
    let gpio = SharedGpio::default();
    let clocks = SharedClocks::default();
    let mut bus = Bus::new();

    let spi1 = SpiSlot::default();
    let mut tim_pwm: Vec<SharedPwm> = Vec::new();
    let timers = [
        ("TIM2", 0x4000_0000, 28, true, 0),
        ("TIM3", 0x4000_0400, 29, false, 1),
        ("TIM4", 0x4000_0800, 30, false, 2),
        ("TIM5", 0x4000_0C00, 50, true, 3),
    ];
    for (name, base, irq, wide, en_bit) in timers {
        let t = GpTimer::new(name, irq, wide, ApbBus::Apb1, en_bit, clocks.clone());
        tim_pwm.push(t.pwm());
        bus.attach(base, 0x400, Box::new(t));
    }
    bus.attach(0x4000_7000, 0x400, Box::new(Pwr::new()));
    bus.attach(0x4001_3000, 0x400, Box::new(Spi::new("SPI1", 35, ApbBus::Apb2, 12, clocks.clone(), spi1.clone())));
    for i in 0..NUM_PORTS {
        bus.attach(GPIOA_BASE + PORT_STRIDE * i as u32, PORT_STRIDE, Box::new(GpioPort::new(i, gpio.clone(), clocks.clone())));
    }
    bus.attach(0x4002_3800, 0x400, Box::new(Rcc::new(clocks.clone())));
    bus.attach(0x4002_3C00, 0x400, Box::new(FlashInterface::new()));
    bus.attach(0x4002_8000, 0x1400, Box::new(EthStub));

    let tim_pwm: [SharedPwm; 4] = tim_pwm.try_into().unwrap_or_else(|_| unreachable!());
    (bus, Stm32f429 { gpio, clocks, spi1, tim_pwm })
}
