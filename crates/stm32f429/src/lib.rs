//! STM32F429ZI: memory map and peripherals.

pub mod adc;
pub mod exti;
pub mod gpio;
pub mod i2c;
pub mod rcc;
pub mod spi;
pub mod timer;
pub mod usart;

use emu_core::{Bus, MachineConfig, MemoryMap};

use gpio::{GPIOA_BASE, GpioPort, NUM_PORTS, PORT_STRIDE, SharedGpio};
use rcc::{FlashInterface, Pwr, Rcc, SharedClocks};
use spi::{Spi, SpiSlot};
use timer::{ApbBus, ChannelPins, GpTimer, SharedPwm};
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

/// Pins of the timer channels (LQFP144) and their alternate function.
const fn pins(af: u32, pins: [&'static [(char, u32)]; 4]) -> ChannelPins {
    ChannelPins { af, pins }
}
static TIM1_PINS: ChannelPins =
    pins(1, [&[('A', 8), ('E', 9)], &[('A', 9), ('E', 11)], &[('A', 10), ('E', 13)], &[('A', 11), ('E', 14)]]);
static TIM2_PINS: ChannelPins =
    pins(1, [&[('A', 0), ('A', 5), ('A', 15)], &[('A', 1), ('B', 3)], &[('A', 2), ('B', 10)], &[('A', 3), ('B', 11)]]);
static TIM3_PINS: ChannelPins = pins(
    2,
    [&[('A', 6), ('B', 4), ('C', 6)], &[('A', 7), ('B', 5), ('C', 7)], &[('B', 0), ('C', 8)], &[('B', 1), ('C', 9)]],
);
static TIM4_PINS: ChannelPins =
    pins(2, [&[('B', 6), ('D', 12)], &[('B', 7), ('D', 13)], &[('B', 8), ('D', 14)], &[('B', 9), ('D', 15)]]);
static TIM5_PINS: ChannelPins = pins(2, [&[('A', 0)], &[('A', 1)], &[('A', 2)], &[('A', 3)]]);
static TIM8_PINS: ChannelPins = pins(3, [&[('C', 6)], &[('C', 7)], &[('C', 8)], &[('C', 9)]]);
static TIM9_PINS: ChannelPins = pins(3, [&[('A', 2), ('E', 5)], &[('A', 3), ('E', 6)], &[], &[]]);
static TIM10_PINS: ChannelPins = pins(3, [&[('B', 8), ('F', 6)], &[], &[], &[]]);
static TIM11_PINS: ChannelPins = pins(3, [&[('B', 9), ('F', 7)], &[], &[], &[]]);
static TIM12_PINS: ChannelPins = pins(9, [&[('B', 14)], &[('B', 15)], &[], &[]]);
static TIM13_PINS: ChannelPins = pins(9, [&[('A', 6), ('F', 8)], &[], &[], &[]]);
static TIM14_PINS: ChannelPins = pins(9, [&[('A', 7), ('F', 9)], &[], &[], &[]]);

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
    /// Devices on I2C1 (the I2C header of the ITS-Board).
    pub i2c1: i2c::I2cBus,
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
        ("TIM2", 0x4000_0000, 28, true, 0, &TIM2_PINS),
        ("TIM3", 0x4000_0400, 29, false, 1, &TIM3_PINS),
        ("TIM4", 0x4000_0800, 30, false, 2, &TIM4_PINS),
        ("TIM5", 0x4000_0C00, 50, true, 3, &TIM5_PINS),
    ];
    for (name, base, irq, wide, en_bit, pins) in timers {
        let t = GpTimer::new(name, irq, wide, ApbBus::Apb1, en_bit, clocks.clone()).with_inputs(pins, gpio.clone());
        tim_pwm.push(t.pwm());
        bus.attach(base, 0x400, Box::new(t));
    }
    let timer = |name, irq, bus, en_bit| GpTimer::new(name, irq, false, bus, en_bit, clocks.clone());
    use ApbBus::{Apb1, Apb2};
    // TIM9..14 share their interrupt lines with TIM1/TIM8 (source 1), TIM6 with the DAC
    let g = || gpio.clone();
    let more = [
        (0x4001_0000, timer("TIM1", 25, Apb2, 0).advanced(27).with_inputs(&TIM1_PINS, g())),
        (0x4001_0400, timer("TIM8", 44, Apb2, 1).advanced(46).with_inputs(&TIM8_PINS, g())),
        (0x4000_1000, timer("TIM6", 54, Apb1, 4).with_channels(0)),
        (0x4000_1400, timer("TIM7", 55, Apb1, 5).with_channels(0)),
        (0x4001_4000, timer("TIM9", 24, Apb2, 16).with_channels(2).shared_irq(1).with_inputs(&TIM9_PINS, g())),
        (0x4001_4400, timer("TIM10", 25, Apb2, 17).with_channels(1).shared_irq(1).with_inputs(&TIM10_PINS, g())),
        (0x4001_4800, timer("TIM11", 26, Apb2, 18).with_channels(1).shared_irq(1).with_inputs(&TIM11_PINS, g())),
        (0x4000_1800, timer("TIM12", 43, Apb1, 6).with_channels(2).shared_irq(1).with_inputs(&TIM12_PINS, g())),
        (0x4000_1C00, timer("TIM13", 44, Apb1, 7).with_channels(1).shared_irq(1).with_inputs(&TIM13_PINS, g())),
        (0x4000_2000, timer("TIM14", 45, Apb1, 8).with_channels(1).shared_irq(1).with_inputs(&TIM14_PINS, g())),
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
    let i2c1 = i2c::I2cBus::default();
    let i2c = [
        ("I2C1", 0x4000_5400, 31, 21, &[('B', 6), ('B', 8)][..], &[('B', 7), ('B', 9)][..], i2c1.clone()),
        (
            "I2C2",
            0x4000_5800,
            33,
            22,
            &[('B', 10), ('F', 1), ('H', 4)],
            &[('B', 11), ('F', 0), ('H', 5)],
            Default::default(),
        ),
        ("I2C3", 0x4000_5C00, 72, 23, &[('A', 8), ('H', 7)], &[('C', 9), ('H', 8)], Default::default()),
    ];
    for (name, base, irq, en_bit, scl, sda, devices) in i2c {
        bus.attach(
            base,
            0x400,
            Box::new(i2c::I2c::new(name, irq, en_bit, clocks.clone(), gpio.clone(), scl, sda, devices)),
        );
    }
    let analog = adc::SharedAnalog::default();
    bus.attach(0x4000_7400, 0x400, Box::new(adc::Dac::new(clocks.clone(), analog.clone())));
    bus.attach(0x4001_2000, 0x400, Box::new(adc::Adc::new(clocks.clone(), gpio.clone(), analog.clone())));
    let exti = exti::SharedExti::default();
    bus.attach(0x4001_3800, 0x400, Box::new(exti::Syscfg::new(exti.clone(), gpio.clone(), clocks.clone())));
    bus.attach(0x4001_3C00, 0x400, Box::new(exti::Exti::new(exti, gpio.clone())));
    bus.attach(0x4001_3000, 0x400, Box::new(Spi::new("SPI1", 35, ApbBus::Apb2, 12, clocks.clone(), spi1.clone())));
    // further SPIs and U(S)ARTs: nothing connected (SPI3 is on the SPI header)
    let spis = [
        ("SPI2", 0x4000_3800, 36, Apb1, 14),
        ("SPI3", 0x4000_3C00, 51, Apb1, 15),
        ("SPI4", 0x4001_3400, 84, Apb2, 13),
        ("SPI5", 0x4001_5000, 85, Apb2, 20),
        ("SPI6", 0x4001_5400, 86, Apb2, 21),
    ];
    for (name, base, irq, apb, en_bit) in spis {
        let spi = Spi::new(name, irq, apb, en_bit, clocks.clone(), SpiSlot::default());
        let spi = if matches!(name, "SPI2" | "SPI3") { spi.with_i2s() } else { spi };
        bus.attach(base, 0x400, Box::new(spi));
    }
    let uarts = [
        ("USART1", 0x4001_1000, 37, Apb2, 4),
        ("USART2", 0x4000_4400, 38, Apb1, 17),
        ("UART4", 0x4000_4C00, 52, Apb1, 19),
        ("UART5", 0x4000_5000, 53, Apb1, 20),
        ("USART6", 0x4001_1400, 71, Apb2, 5),
        ("UART7", 0x4000_7800, 82, Apb1, 30),
        ("UART8", 0x4000_7C00, 83, Apb1, 31),
    ];
    for (name, base, irq, apb, en_bit) in uarts {
        let port = SharedSerial::default();
        bus.attach(base, 0x400, Box::new(Usart::new(name, irq, apb, en_bit, clocks.clone(), port)));
    }
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
    (bus, Stm32f429 { gpio, clocks, spi1, tim_pwm, usart3, analog, i2c1 })
}
