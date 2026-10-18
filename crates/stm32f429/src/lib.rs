//! STM32F429ZI: memory map and peripherals.

pub mod adc;
pub mod crc;
pub mod dma;
pub mod exti;
pub mod flash;
pub mod gpio;
pub mod i2c;
pub mod iwdg;
pub mod rcc;
pub mod spi;
pub mod timer;
pub mod usart;

use emu_core::{Bus, DebugIds, Machine, MachineConfig, MachineError, MemoryMap};

use dma::DmaLine;
use gpio::{GPIOA_BASE, GpioPort, NUM_PORTS, PORT_STRIDE, SharedGpio};
use rcc::{Pwr, Rcc, SharedClocks};
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
    /// DMA controllers; the machine runs them as its second bus master.
    pub dma: dma::SharedDma,
    /// State of the flash interface (registers and programming side).
    pub flash: flash::SharedFlash,
}

/// What an SPI master receives with no device on the bus: the idle level of
/// its MISO pin (on the board, the open SPI3 header reads 0x00).
struct OpenMiso {
    gpio: SharedGpio,
    pins: &'static [(char, u32)],
}

impl spi::SpiDevice for OpenMiso {
    fn transfer(&mut self, _mosi: u16) -> u16 {
        let g = self.gpio.borrow();
        let pin = self.pins.iter().find(|&&(p, pin)| g.ports[gpio::port(p)].mode(pin) == gpio::PinMode::Alternate);
        match pin {
            Some(&(p, pin)) if g.level(gpio::port(p), pin) => 0xFFFF,
            Some(_) => 0,
            None => 0xFFFF,
        }
    }
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

/// The emulated MCU: core and memory with `bus` (from [`build`]), the DMA
/// controllers of `mcu` as second bus master.
pub fn machine(bus: Bus, mcu: &Stm32f429) -> Result<Machine, MachineError> {
    let mut m = Machine::new(machine_config(), bus)?;
    m.set_bus_master(Box::new(dma::DmaMaster(mcu.dma.clone())));
    m.set_debug_ids(DEBUG_IDS);
    m.set_flash_controller(Box::new(flash::FlashProgrammer::new(mcu.flash.clone(), mcu.clocks.clone())));
    // Device electronic signature in system memory (read-only): unique ID
    // (an arbitrary one) and flash size in KB
    let mut signature = [0u8; 0x14];
    signature[..12].copy_from_slice(&[0x2E, 0x00, 0x3A, 0x00, 0x0B, 0x51, 0x33, 0x34, 0x34, 0x33, 0x37, 0x31]);
    signature[0x12..].copy_from_slice(&((FLASH_SIZE / 1024) as u16).to_le_bytes());
    m.write_mem(UID_BASE, &signature);
    Ok(m)
}

/// Unique device ID (96 bits); the flash size register follows at +0x12.
pub const UID_BASE: u32 = 0x1FFF_7A10;

/// DBGMCU_IDCODE: DEV_ID 0x419 (STM32F42x/43x), REV_ID 0x2001 (revision 3).
pub const DBGMCU_IDCODE: u32 = 0x2001_6419;

/// ST's parts of the debug system: the ROM table carries the device ID as
/// part number and ST's JEP106 code (bank 0, 0x20).
pub const DEBUG_IDS: DebugIds = DebugIds {
    rom_table_part: 0x419,
    rom_table_jep106: (0, 0x20),
    rom_table_revision: 0,
    dbgmcu_idcode: Some(DBGMCU_IDCODE),
};

/// Builds the peripheral bus with all modelled STM32F429 peripherals.
pub fn build() -> (Bus, Stm32f429) {
    let gpio = SharedGpio::default();
    let clocks = SharedClocks::default();
    let mut bus = Bus::new();
    let dma = dma::SharedDma::default();
    let triggers = timer::SharedTriggers::default();

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
        let trgo = [timer::TRGO_TIM2, timer::TRGO_TIM3, timer::TRGO_TIM4, timer::TRGO_TIM5][en_bit as usize];
        let t = t.with_trgo(triggers.clone(), trgo);
        tim_pwm.push(t.pwm());
        bus.attach(base, 0x400, Box::new(t));
    }
    let timer = |name, irq, bus, en_bit| GpTimer::new(name, irq, false, bus, en_bit, clocks.clone());
    use ApbBus::{Apb1, Apb2};
    // TIM9..14 share their interrupt lines with TIM1/TIM8 (source 1), TIM6 with the DAC
    let g = || gpio.clone();
    let more = [
        (0x4001_0000, timer("TIM1", 25, Apb2, 0).advanced(27).with_inputs(&TIM1_PINS, g())),
        (
            0x4001_0400,
            timer("TIM8", 44, Apb2, 1)
                .advanced(46)
                .with_inputs(&TIM8_PINS, g())
                .with_trgo(triggers.clone(), timer::TRGO_TIM8),
        ),
        (0x4000_1000, timer("TIM6", 54, Apb1, 4).with_channels(0).with_trgo(triggers.clone(), timer::TRGO_TIM6)),
        (0x4000_1400, timer("TIM7", 55, Apb1, 5).with_channels(0).with_trgo(triggers.clone(), timer::TRGO_TIM7)),
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
    let usart3_dma = (DmaLine(&[(0, 1, 4)]), DmaLine(&[(0, 3, 4), (0, 4, 7)]));
    bus.attach(
        0x4000_4800,
        0x400,
        Box::new(Usart::new("USART3", 39, ApbBus::Apb1, 18, clocks.clone(), usart3.clone()).with_dma(
            dma.clone(),
            usart3_dma.0,
            usart3_dma.1,
        )),
    );
    bus.attach(0x4000_7000, 0x400, Box::new(Pwr::new()));
    bus.attach(0x4000_3000, 0x400, Box::new(iwdg::Iwdg::new()));
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
    bus.attach(
        0x4000_7400,
        0x400,
        Box::new(adc::Dac::new(clocks.clone(), analog.clone(), dma.clone(), triggers.clone())),
    );
    bus.attach(
        0x4001_2000,
        0x400,
        Box::new(adc::Adc::new(clocks.clone(), gpio.clone(), analog.clone(), dma.clone(), triggers.clone())),
    );
    let exti = exti::SharedExti::default();
    bus.attach(0x4001_3800, 0x400, Box::new(exti::Syscfg::new(exti.clone(), gpio.clone(), clocks.clone())));
    bus.attach(0x4001_3C00, 0x400, Box::new(exti::Exti::new(exti, gpio.clone())));
    let spi1_dma = (DmaLine(&[(1, 0, 3), (1, 2, 3)]), DmaLine(&[(1, 3, 3), (1, 5, 3)]));
    let spi = Spi::new("SPI1", 35, ApbBus::Apb2, 12, clocks.clone(), spi1.clone());
    bus.attach(0x4001_3000, 0x400, Box::new(spi.with_dma(dma.clone(), spi1_dma.0, spi1_dma.1)));
    // further SPIs and U(S)ARTs: nothing connected (SPI3 is on the SPI header)
    // DMA request lines (controller, stream, channel) and MISO pins
    let spis: [(_, _, _, _, _, &'static [_], &'static [_], &'static [_]); 5] = [
        ("SPI2", 0x4000_3800, 36, Apb1, 14, &[(0, 3, 0)], &[(0, 4, 0)], &[('B', 14), ('C', 2)]),
        ("SPI3", 0x4000_3C00, 51, Apb1, 15, &[(0, 0, 0), (0, 2, 0)], &[(0, 5, 0), (0, 7, 0)], &[('C', 11), ('B', 4)]),
        ("SPI4", 0x4001_3400, 84, Apb2, 13, &[(1, 0, 4), (1, 3, 5)], &[(1, 1, 4), (1, 4, 5)], &[('E', 5), ('E', 13)]),
        ("SPI5", 0x4001_5000, 85, Apb2, 20, &[(1, 3, 2), (1, 5, 7)], &[(1, 4, 2), (1, 6, 7)], &[('F', 8)]),
        ("SPI6", 0x4001_5400, 86, Apb2, 21, &[(1, 6, 1)], &[(1, 5, 1)], &[('G', 12)]),
    ];
    for (name, base, irq, apb, en_bit, rx, tx, miso) in spis {
        let open: Box<dyn spi::SpiDevice> = Box::new(OpenMiso { gpio: gpio.clone(), pins: miso });
        let spi =
            Spi::new(name, irq, apb, en_bit, clocks.clone(), std::rc::Rc::new(std::cell::RefCell::new(Some(open))))
                .with_dma(dma.clone(), DmaLine(rx), DmaLine(tx));
        let spi = if matches!(name, "SPI2" | "SPI3") { spi.with_i2s() } else { spi };
        bus.attach(base, 0x400, Box::new(spi));
    }
    // DMA request lines (controller, stream, channel) for reception / transmission
    let uarts: [(_, _, _, _, _, &'static [_], &'static [_]); 7] = [
        ("USART1", 0x4001_1000, 37, Apb2, 4, &[(1, 2, 4), (1, 5, 4)], &[(1, 7, 4)]),
        ("USART2", 0x4000_4400, 38, Apb1, 17, &[(0, 5, 4)], &[(0, 6, 4)]),
        ("UART4", 0x4000_4C00, 52, Apb1, 19, &[(0, 2, 4)], &[(0, 4, 4)]),
        ("UART5", 0x4000_5000, 53, Apb1, 20, &[(0, 0, 4)], &[(0, 7, 4)]),
        ("USART6", 0x4001_1400, 71, Apb2, 5, &[(1, 1, 5), (1, 2, 5)], &[(1, 6, 5), (1, 7, 5)]),
        ("UART7", 0x4000_7800, 82, Apb1, 30, &[(0, 3, 5)], &[(0, 1, 5)]),
        ("UART8", 0x4000_7C00, 83, Apb1, 31, &[(0, 6, 5)], &[(0, 0, 5)]),
    ];
    for (name, base, irq, apb, en_bit, rx, tx) in uarts {
        let port = SharedSerial::default();
        let u =
            Usart::new(name, irq, apb, en_bit, clocks.clone(), port).with_dma(dma.clone(), DmaLine(rx), DmaLine(tx));
        bus.attach(base, 0x400, Box::new(u));
    }
    for i in 0..NUM_PORTS {
        bus.attach(
            GPIOA_BASE + PORT_STRIDE * i as u32,
            PORT_STRIDE,
            Box::new(GpioPort::new(i, gpio.clone(), clocks.clone())),
        );
    }
    bus.attach(0x4002_3000, 0x400, Box::new(crc::Crc::new(clocks.clone())));
    bus.attach(0x4002_6000, 0x400, Box::new(dma::DmaRegs::new(0, dma.clone(), clocks.clone())));
    bus.attach(0x4002_6400, 0x400, Box::new(dma::DmaRegs::new(1, dma.clone(), clocks.clone())));
    bus.attach(0x4002_3800, 0x400, Box::new(Rcc::new(clocks.clone())));
    bus.attach(0x5006_0800, 0x400, Box::new(crc::Rng::new(clocks.clone())));
    let flash = flash::SharedFlash::default();
    bus.attach(0x4002_3C00, 0x400, Box::new(flash::FlashInterface::new(flash.clone())));
    bus.attach(0x4002_8000, 0x1400, Box::new(EthStub));
    bus.set_wait_states(Box::new(bus_wait_states(clocks.clone())));

    let tim_pwm: [SharedPwm; 4] = tim_pwm.try_into().unwrap_or_else(|_| unreachable!());
    (bus, Stm32f429 { gpio, clocks, spi1, tim_pwm, usart3, analog, i2c1, dma, flash })
}
