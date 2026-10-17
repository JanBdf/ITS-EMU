//! STM32F429ZI: memory map and peripherals.

pub mod adc;
pub mod exti;
pub mod gpio;
pub mod rcc;
pub mod spi;
pub mod timer;
pub mod usart;

use emu_core::{Bus, MachineConfig, MemoryMap};

use gpio::{GPIOA_BASE, GpioPort, NUM_PORTS, PORT_STRIDE, SharedGpio};
use rcc::{FlashInterface, Pwr, Rcc, SharedClocks};
use spi::{Spi, SpiSlot};
use timer::{ApbBus, GpTimer, SharedPwm};
use usart::{SharedSerial, Usart};

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
    /// USART3, connected to the ST-LINK virtual COM port on the Nucleo board.
    pub usart3: SharedSerial,
    /// Voltages on the analog inputs and the DAC outputs.
    pub analog: adc::SharedAnalog,
}

/// Ethernet MAC/DMA registers: reads as idle, writes ignored. The LCD driver
/// briefly switches the MAC off around every SPI transfer, so the window must
/// exist (and stay quiet) even though Ethernet is not emulated.
struct EthStub;

impl emu_core::Peripheral for EthStub {
    fn name(&self) -> &str {
        "ETH"
    }

    fn read_stable_until(&self, _offset: u32, _clock: &emu_core::Clock) -> u64 {
        u64::MAX
    }
    fn read(&mut self, _: &mut emu_core::DeviceCtx, _: u32, _: u32) -> u32 {
        0
    }
    fn write(&mut self, _: &mut emu_core::DeviceCtx, _: u32, _: u32, _: u32) {}
}

/// Wait states of peripheral accesses, fitted to DWT->CYCCNT measurements at
/// 180 MHz (HwCheck `cyc`): back-to-back APB reads take 2 APB clocks
/// (2·div - 1 extra core cycles), writes one cycle less; the first access
/// after other work also waits for the bridge (div cycles for reads,
/// 2·div - 1 for writes). AHB peripherals cost about one cycle more than SRAM.
fn bus_wait_states(clocks: SharedClocks) -> impl Fn(u32, bool, u64) -> u64 {
    let last_end = std::cell::Cell::new(u64::MAX);
    move |addr, write, now| {
        let c = clocks.get();
        let pclk = match addr {
            0x4000_0000..=0x4000_7FFF => c.pclk1,
            0x4001_0000..=0x4001_6BFF => c.pclk2,
            0x4002_0000..=0x5FFF_FFFF => return 1,
            _ => return 0,
        };
        let div = u64::from(c.hclk / pclk.max(1)).max(1);
        if div == 1 {
            return 0;
        }
        let mut ws = if write { 2 * div - 2 } else { 2 * div - 1 };
        if now != last_end.get() {
            ws += if write { 2 * div - 1 } else { div };
        }
        last_end.set(now + ws);
        ws
    }
}

/// Builds the peripheral bus with all modelled STM32F429 peripherals.
pub fn build() -> (Bus, Stm32f429) {
    let gpio = SharedGpio::default();
    let clocks = SharedClocks::default();
    let mut bus = Bus::new();

    let spi1 = SpiSlot::default();
    let usart3 = SharedSerial::default();
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
    let timer = |name, irq, bus, en_bit| GpTimer::new(name, irq, false, bus, en_bit, clocks.clone());
    use ApbBus::{Apb1, Apb2};
    // TIM9..14 share their interrupt lines with TIM1/TIM8 (source 1), TIM6 with the DAC
    let more = [
        (0x4001_0000, timer("TIM1", 25, Apb2, 0).advanced(27)),
        (0x4001_0400, timer("TIM8", 44, Apb2, 1).advanced(46)),
        (0x4000_1000, timer("TIM6", 54, Apb1, 4).with_channels(0)),
        (0x4000_1400, timer("TIM7", 55, Apb1, 5).with_channels(0)),
        (0x4001_4000, timer("TIM9", 24, Apb2, 16).with_channels(2).shared_irq(1)),
        (0x4001_4400, timer("TIM10", 25, Apb2, 17).with_channels(1).shared_irq(1)),
        (0x4001_4800, timer("TIM11", 26, Apb2, 18).with_channels(1).shared_irq(1)),
        (0x4000_1800, timer("TIM12", 43, Apb1, 6).with_channels(2).shared_irq(1)),
        (0x4000_1C00, timer("TIM13", 44, Apb1, 7).with_channels(1).shared_irq(1)),
        (0x4000_2000, timer("TIM14", 45, Apb1, 8).with_channels(1).shared_irq(1)),
    ];
    for (base, t) in more {
        bus.attach(base, 0x400, Box::new(t));
    }
    bus.attach(
        0x4000_4800,
        0x400,
        Box::new(Usart::new("USART3", 39, ApbBus::Apb1, 18, clocks.clone(), usart3.clone())),
    );
    bus.attach(0x4000_7000, 0x400, Box::new(Pwr::new()));
    let analog = adc::SharedAnalog::default();
    bus.attach(0x4000_7400, 0x400, Box::new(adc::Dac::new(clocks.clone(), analog.clone())));
    bus.attach(0x4001_2000, 0x400, Box::new(adc::Adc::new(clocks.clone(), gpio.clone(), analog.clone())));
    let exti = exti::SharedExti::default();
    bus.attach(0x4001_3800, 0x400, Box::new(exti::Syscfg::new(exti.clone(), gpio.clone(), clocks.clone())));
    bus.attach(0x4001_3C00, 0x400, Box::new(exti::Exti::new(exti, gpio.clone())));
    bus.attach(0x4001_3000, 0x400, Box::new(Spi::new("SPI1", 35, ApbBus::Apb2, 12, clocks.clone(), spi1.clone())));
    for i in 0..NUM_PORTS {
        bus.attach(
            GPIOA_BASE + PORT_STRIDE * i as u32,
            PORT_STRIDE,
            Box::new(GpioPort::new(i, gpio.clone(), clocks.clone())),
        );
    }
    bus.attach(0x4002_3800, 0x400, Box::new(Rcc::new(clocks.clone())));
    bus.attach(0x4002_3C00, 0x400, Box::new(FlashInterface::new()));
    bus.attach(0x4002_8000, 0x1400, Box::new(EthStub));
    bus.set_wait_states(Box::new(bus_wait_states(clocks.clone())));

    let tim_pwm: [SharedPwm; 4] = tim_pwm.try_into().unwrap_or_else(|_| unreachable!());
    (bus, Stm32f429 { gpio, clocks, spi1, tim_pwm, usart3, analog })
}
