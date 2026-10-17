//! External devices on the header pins (rotary encoder, ...). They drive and
//! watch pins through the GPIO bank and take part in the machine's time
//! keeping as a device without registers.

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::ResetKind;
use emu_core::{DeviceCtx, Peripheral};
use stm32f429::gpio::{SharedGpio, port};

/// A device on the pins: like a peripheral, but without registers.
pub trait PinDevice {
    /// Brings the device up to `ctx.now()`.
    fn update(&mut self, ctx: &mut DeviceCtx);
    /// Cycle of the device's next own action.
    fn next_event(&self) -> Option<u64>;
    /// The MCU was reset (the device itself keeps running).
    fn mcu_reset(&mut self) {}
}

pub type SharedDevices = Rc<RefCell<Vec<Rc<RefCell<dyn PinDevice>>>>>;

/// The devices as one [`Peripheral`] for [`emu_core::Bus::attach_device`].
pub struct DeviceHub(pub SharedDevices);

impl Peripheral for DeviceHub {
    fn name(&self) -> &str {
        "external devices"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, _offset: u32, _size: u32) -> u32 {
        0
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, _offset: u32, _size: u32, _value: u32) {}

    fn update(&mut self, ctx: &mut DeviceCtx) {
        for d in self.0.borrow().iter() {
            d.borrow_mut().update(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.0.borrow().iter().filter_map(|d| d.borrow().next_event()).min()
    }

    fn system_reset(&mut self, _kind: ResetKind) {
        for d in self.0.borrow().iter() {
            d.borrow_mut().mcu_reset();
        }
    }
}

/// Quadrature levels (A, B) in clockwise order; at rest (detent) both
/// contacts are open and the module's pull-ups hold the lines high.
const QUADRATURE: [(bool, bool); 4] = [(true, true), (false, true), (false, false), (true, false)];
/// Transitions per detent (one full quadrature cycle).
pub const TRANSITIONS_PER_DETENT: i32 = 4;
/// Time between two transitions while the knob turns.
const TRANSITION_NS: u64 = 1_000_000;

/// Mechanical rotary encoder with contacts to GND on two pins (A, B) and
/// pull-ups on the module. Clockwise, A falls before B: a timer in encoder
/// mode counts up.
pub struct RotaryEncoder {
    gpio: SharedGpio,
    a: (usize, u32),
    b: (usize, u32),
    state: usize,
    /// Transitions still to do (positive: clockwise).
    pending: i32,
    /// Cycle of the next transition (0: as soon as possible).
    next_at: Option<u64>,
}

impl RotaryEncoder {
    pub fn new(gpio: SharedGpio, a: (char, u32), b: (char, u32)) -> Self {
        let mut e = Self { gpio, a: (port(a.0), a.1), b: (port(b.0), b.1), state: 0, pending: 0, next_at: None };
        e.drive(0);
        e
    }

    /// Turns the knob by `detents` (negative: counter-clockwise).
    pub fn rotate(&mut self, detents: i32) {
        self.pending += detents * TRANSITIONS_PER_DETENT;
        if self.pending != 0 && self.next_at.is_none() {
            self.next_at = Some(0);
        }
    }

    /// Detents still to turn.
    pub fn turning(&self) -> bool {
        self.pending != 0
    }

    fn drive(&mut self, cycle: u64) {
        let (a, b) = QUADRATURE[self.state];
        let mut g = self.gpio.borrow_mut();
        g.drive_at(self.a.0, self.a.1, Some(a), cycle);
        g.drive_at(self.b.0, self.b.1, Some(b), cycle);
    }
}

impl PinDevice for RotaryEncoder {
    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        while let Some(at) = self.next_at
            && at <= now
        {
            let at = if at == 0 { now } else { at };
            self.state = (self.state as i32 + self.pending.signum()).rem_euclid(4) as usize;
            self.pending -= self.pending.signum();
            self.drive(at);
            self.next_at = (self.pending != 0).then(|| at + ctx.clock.ns_to_cycles(TRANSITION_NS).max(1));
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.next_at
    }
}

/// CRC-8 of the 1-Wire devices (Dallas/Maxim: x^8 + x^5 + x^4 + 1, LSB first).
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0, |mut crc, &byte| {
        let mut b = byte;
        for _ in 0..8 {
            let mix = (crc ^ b) & 1;
            crc >>= 1;
            if mix != 0 {
                crc ^= 0x8C;
            }
            b >>= 1;
        }
        crc
    })
}

/// What the next time slots of the bus master mean to the sensor.
#[derive(Debug, Clone)]
enum Slots {
    /// Not addressed: wait for the next reset.
    Idle,
    /// Bits written by the master (LSB first), then `then`.
    Receive { value: u64, n: u32, need: u32, then: Received },
    /// Bits for the master's read slots.
    Send(std::collections::VecDeque<bool>),
    /// Read slots answer 0 while busy (conversion, EEPROM copy), then 1.
    Busy,
    /// Read slots answer 1 (externally powered).
    Powered,
    /// Search ROM: bit `i`, step 0 = send the bit, 1 = its complement,
    /// 2 = receive the master's choice.
    Search { i: u32, step: u8 },
}

#[derive(Debug, Clone, Copy)]
enum Received {
    RomCommand,
    MatchRom,
    FunctionCommand,
    WriteScratchpad,
}

#[derive(Debug, Clone, Copy)]
enum Action {
    /// Pull the line low until `Release`.
    Pull,
    Release,
    /// Sample a write slot.
    Sample,
}

/// Low time after which the sensor takes a pulse as reset (the master
/// sends at least 480 µs; slots are at most 120 µs).
const RESET_MIN_NS: u64 = 400_000;
/// Presence pulse after the reset: 30 µs after the line rises, 120 µs long.
const PRESENCE_WAIT_NS: u64 = 30_000;
const PRESENCE_NS: u64 = 120_000;
/// Write slots are sampled 30 µs after the falling edge; a 0 bit in a read
/// slot holds the line low for 30 µs (datasheet: 15..60 µs).
const SLOT_SAMPLE_NS: u64 = 30_000;
const COPY_NS: u64 = 10_000_000;

/// DS18B20 temperature sensor (external power) on a [`OneWireBus`].
/// Power-on values, conversion times (maximum per resolution) and the read
/// slots while busy follow the datasheet.
pub struct Ds18b20 {
    rom: [u8; 8],
    /// Temperature at the sensor (°C).
    celsius: f32,
    temperature: u16,
    th: u8,
    tl: u8,
    config: u8,
    eeprom: [u8; 3],
    slots: Slots,
    busy_until: u64,
    conversion_pending: bool,
    /// The master pulled the line low at this cycle (start of a slot or reset).
    fell_at: Option<u64>,
    actions: Vec<(u64, Action)>,
    pulling: bool,
}

impl Ds18b20 {
    /// A sensor with the 48-bit serial number `serial`.
    pub fn new(serial: u64) -> Self {
        let mut rom = [0u8; 8];
        rom[0] = 0x28; // family code
        rom[1..7].copy_from_slice(&serial.to_le_bytes()[..6]);
        rom[7] = crc8(&rom[..7]);
        let eeprom = [0x4B, 0x46, 0x7F];
        Self {
            rom,
            celsius: 21.0,
            temperature: 0x0550, // 85 °C until the first conversion
            th: eeprom[0],
            tl: eeprom[1],
            config: eeprom[2],
            eeprom,
            slots: Slots::Idle,
            busy_until: 0,
            conversion_pending: false,
            fell_at: None,
            actions: Vec::new(),
            pulling: false,
        }
    }

    pub fn rom(&self) -> [u8; 8] {
        self.rom
    }

    /// Sets the temperature the next conversion measures.
    pub fn set_celsius(&mut self, celsius: f32) {
        self.celsius = celsius.clamp(-55.0, 125.0);
    }

    fn scratchpad(&self) -> [u8; 9] {
        let [lo, hi] = self.temperature.to_le_bytes();
        let mut pad = [lo, hi, self.th, self.tl, self.config, 0xFF, 0x0C, 0x10, 0];
        pad[8] = crc8(&pad[..8]);
        pad
    }

    fn resolution_bits(&self) -> u32 {
        9 + u32::from((self.config >> 5) & 3)
    }

    fn schedule(&mut self, at: u64, action: Action) {
        self.actions.push((at, action));
        self.actions.sort_by_key(|&(t, _)| t);
    }

    fn receive(need: u32, then: Received) -> Slots {
        Slots::Receive { value: 0, n: 0, need, then }
    }

    fn send_bytes(bytes: &[u8]) -> Slots {
        Slots::Send(bytes.iter().flat_map(|b| (0..8).map(move |i| b >> i & 1 != 0)).collect())
    }

    /// A complete value written by the master.
    fn received(&mut self, value: u64, then: Received, ctx: &DeviceCtx) {
        self.slots = match then {
            Received::RomCommand => match value as u8 {
                0x33 => Self::send_bytes(&self.rom),
                0xCC => Self::receive(8, Received::FunctionCommand),
                0x55 => Self::receive(64, Received::MatchRom),
                0xF0 => Slots::Search { i: 0, step: 0 },
                0xEC if self.alarm() => Slots::Search { i: 0, step: 0 },
                _ => Slots::Idle,
            },
            Received::MatchRom if value == u64::from_le_bytes(self.rom) => Self::receive(8, Received::FunctionCommand),
            Received::MatchRom => Slots::Idle,
            Received::FunctionCommand => match value as u8 {
                0x44 => {
                    let ns = 93_750_000u64 << (self.resolution_bits() - 9);
                    self.busy_until = ctx.now() + ctx.clock.ns_to_cycles(ns);
                    self.conversion_pending = true;
                    Slots::Busy
                }
                0xBE => Self::send_bytes(&self.scratchpad()),
                0x4E => Self::receive(24, Received::WriteScratchpad),
                0x48 => {
                    self.eeprom = [self.th, self.tl, self.config];
                    self.busy_until = ctx.now() + ctx.clock.ns_to_cycles(COPY_NS);
                    Slots::Busy
                }
                0xB8 => {
                    [self.th, self.tl, self.config] = self.eeprom;
                    Slots::Powered
                }
                0xB4 => Slots::Powered,
                _ => Slots::Idle,
            },
            Received::WriteScratchpad => {
                let [th, tl, config, ..] = value.to_le_bytes();
                (self.th, self.tl, self.config) = (th, tl, (config & 0x60) | 0x1F);
                Slots::Idle
            }
        };
    }

    fn alarm(&self) -> bool {
        let t = (self.temperature as i16 >> 4) as i8;
        t > self.th as i8 || t <= self.tl as i8
    }

    /// Ends a conversion: the temperature register gets the measurement,
    /// rounded to the resolution (the undefined low bits read 0).
    fn finish_conversion(&mut self, now: u64) {
        if self.conversion_pending && now >= self.busy_until {
            self.conversion_pending = false;
            let raw = (self.celsius * 16.0).round() as i16;
            let undefined = (1i16 << (12 - self.resolution_bits())) - 1;
            self.temperature = (raw & !undefined) as u16;
        }
    }

    /// The master pulled the line low at `t`: a time slot starts.
    fn slot(&mut self, t: u64, ctx: &DeviceCtx) {
        let hold = t + ctx.clock.ns_to_cycles(SLOT_SAMPLE_NS);
        // the bit this sensor sends, if the slot is a read slot
        let send = match &mut self.slots {
            Slots::Idle => return,
            Slots::Receive { .. } => None,
            Slots::Send(bits) => {
                let bit = bits.pop_front();
                if bits.is_empty() {
                    self.slots = Slots::Idle;
                }
                bit
            }
            Slots::Busy => Some(t >= self.busy_until),
            Slots::Powered => Some(true),
            Slots::Search { i, step } => {
                let bit = self.rom[(*i / 8) as usize] >> (*i % 8) & 1 != 0;
                match *step {
                    0 => {
                        *step = 1;
                        Some(bit)
                    }
                    1 => {
                        *step = 2;
                        Some(!bit)
                    }
                    _ => None,
                }
            }
        };
        match send {
            Some(false) => {
                self.pulling = true;
                self.schedule(hold, Action::Release);
            }
            Some(true) => {}
            None => self.schedule(hold, Action::Sample),
        }
    }

    /// A write slot sampled the line.
    fn sampled(&mut self, bit: bool, ctx: &DeviceCtx) {
        match &mut self.slots {
            Slots::Receive { value, n, need, then } => {
                *value |= u64::from(bit) << *n;
                *n += 1;
                if *n == *need {
                    let (value, then) = (*value, *then);
                    self.received(value, then, ctx);
                }
            }
            Slots::Search { i, step } if *step == 2 => {
                let mine = self.rom[(*i / 8) as usize] >> (*i % 8) & 1 != 0;
                if bit != mine {
                    self.slots = Slots::Idle;
                } else if *i == 63 {
                    self.slots = Self::receive(8, Received::FunctionCommand);
                } else {
                    *i += 1;
                    *step = 0;
                }
            }
            _ => {}
        }
    }
}

impl Ds18b20 {
    /// The master changed the line level at `t`: a falling edge starts a time
    /// slot, a rising one after a long low time ends a reset pulse.
    fn master_edge(&mut self, level: bool, t: u64, ctx: &DeviceCtx) {
        if !level {
            self.fell_at = Some(t);
            self.slot(t, ctx);
        } else if let Some(fell) = self.fell_at.take()
            && t - fell >= ctx.clock.ns_to_cycles(RESET_MIN_NS)
        {
            // reset: forget everything, answer with a presence pulse
            self.actions.clear();
            self.pulling = false;
            let start = t + ctx.clock.ns_to_cycles(PRESENCE_WAIT_NS);
            self.schedule(start, Action::Pull);
            self.schedule(start + ctx.clock.ns_to_cycles(PRESENCE_NS), Action::Release);
            self.slots = Self::receive(8, Received::RomCommand);
        }
    }

    fn next_action(&self) -> Option<u64> {
        self.actions.first().map(|&(t, _)| t)
    }

    /// Runs the earliest action; `line` is the bus level at its time.
    fn act(&mut self, line: bool, ctx: &DeviceCtx) {
        let (_, action) = self.actions.remove(0);
        match action {
            Action::Pull => self.pulling = true,
            Action::Release => self.pulling = false,
            Action::Sample => self.sampled(line, ctx),
        }
    }

    fn mcu_reset(&mut self) {
        self.actions.clear();
        self.pulling = false;
        self.slots = Slots::Idle;
        self.fell_at = None;
    }
}

/// A 1-Wire line on a pin with a pull-up and the sensors on it: they see the
/// master's edges and pull the line low together (wired AND).
pub struct OneWireBus {
    gpio: SharedGpio,
    pin: (usize, u32),
    pub sensors: Vec<Ds18b20>,
    low: bool,
}

impl OneWireBus {
    pub fn new(gpio: SharedGpio, pin: (char, u32)) -> Self {
        let pin = (port(pin.0), pin.1);
        {
            let mut g = gpio.borrow_mut();
            g.ports[pin.0].log_watch |= 1 << pin.1;
            g.drive_at(pin.0, pin.1, Some(true), 0); // the pull-up
        }
        Self { gpio, pin, sensors: Vec::new(), low: false }
    }

    pub fn pin(&self) -> (usize, u32) {
        self.pin
    }

    /// Drives the line low while any sensor pulls.
    fn drive(&mut self, at: u64) {
        let low = self.sensors.iter().any(|s| s.pulling);
        if low != self.low {
            self.low = low;
            self.gpio.borrow_mut().drive_at(self.pin.0, self.pin.1, Some(!low), at);
        }
    }
}

impl PinDevice for OneWireBus {
    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        for s in &mut self.sensors {
            s.finish_conversion(now);
        }
        // the master's edges (the sensors' own ones are marked external)
        let edges = self.gpio.borrow_mut().take_edges(self.pin.0, 1 << self.pin.1);
        for (_, level, cycle, external) in edges {
            if !external {
                let t = if cycle == 0 { now } else { cycle };
                for s in &mut self.sensors {
                    s.master_edge(level, t, ctx);
                }
                self.drive(now);
            }
        }
        // timed actions of all sensors in order
        while let Some((at, i)) = self.sensors.iter().enumerate().filter_map(|(i, s)| Some((s.next_action()?, i))).min()
            && at <= now
        {
            let line = self.gpio.borrow().level(self.pin.0, self.pin.1);
            self.sensors[i].act(line, ctx);
            self.drive(at);
        }
    }

    fn next_event(&self) -> Option<u64> {
        let actions = self.sensors.iter().filter_map(Ds18b20::next_action);
        let conversions = self.sensors.iter().filter(|s| s.conversion_pending).map(|s| s.busy_until);
        actions.chain(conversions).min()
    }

    fn mcu_reset(&mut self) {
        for s in &mut self.sensors {
            s.mcu_reset();
        }
        self.drive(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_of_the_power_on_scratchpad() {
        // DS18B20 scratchpad after power-on: 85 °C, TH 0x4B, TL 0x46, 12 bit
        assert_eq!(crc8(&[0x50, 0x05, 0x4B, 0x46, 0x7F, 0xFF, 0x0C, 0x10]), 0x1C);
        // a complete block including its CRC gives 0
        assert_eq!(crc8(&[0x50, 0x05, 0x4B, 0x46, 0x7F, 0xFF, 0x0C, 0x10, 0x1C]), 0);
    }

    use emu_core::Clock;
    use emu_core::nvic::Nvic;
    use stm32f429::gpio::{GpioBank, GpioPort};
    use stm32f429::rcc::{Clocks, SharedClocks};

    /// A bit-banging master on PG2 (open drain), independent of HwCheck's,
    /// with timings at the edges of the datasheet's windows.
    struct Master {
        gpio: SharedGpio,
        port: GpioPort,
        bus: OneWireBus,
        clock: Clock,
        nvic: Nvic,
    }

    impl Master {
        fn new(sensors: u64) -> Self {
            let gpio: SharedGpio = Rc::new(RefCell::new(GpioBank::default()));
            let clocks = SharedClocks::default();
            clocks.set(Clocks { hclk: 180_000_000, pclk2: 90_000_000, ahb1enr: 1 << 6, ..Clocks::default() });
            let port = GpioPort::new(6, gpio.clone(), clocks);
            let mut bus = OneWireBus::new(gpio.clone(), ('G', 2));
            for n in 0..sensors {
                bus.sensors.push(Ds18b20::new(0x1234_5678 + 0x9_0000 * n));
            }
            let mut m = Self { gpio, port, bus, clock: Clock::new(180_000_000), nvic: Nvic::new() };
            m.reg(0x14, 1 << 2); // ODR: released
            m.reg(0x04, 1 << 2); // open drain
            m.reg(0x00, 1 << 4); // output
            m
        }

        fn reg(&mut self, off: u32, v: u32) {
            let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
            self.port.write(&mut ctx, off, 4, v);
            self.bus.update(&mut ctx);
        }

        fn wait_us(&mut self, us: f64) {
            let target = self.clock.cycles() + (us * 180.0) as u64;
            loop {
                let next = self.bus.next_event().filter(|&t| t <= target).unwrap_or(target);
                self.clock.advance(next.saturating_sub(self.clock.cycles()));
                let mut ctx = DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic };
                self.bus.update(&mut ctx);
                if self.clock.cycles() >= target {
                    break;
                }
            }
        }

        fn low(&mut self, us: f64) {
            self.reg(0x18, 1 << (2 + 16));
            self.wait_us(us);
            self.reg(0x18, 1 << 2);
        }

        fn line(&self) -> bool {
            self.gpio.borrow().level(6, 2)
        }

        fn reset(&mut self) -> bool {
            self.low(500.0);
            self.wait_us(100.0); // late in the presence pulse (60..240 µs)
            let presence = !self.line();
            self.wait_us(400.0);
            presence
        }

        fn write_bit(&mut self, bit: bool) {
            // 1: shortest low pulse, 0: longest slot; shortest recovery
            self.low(if bit { 1.0 } else { 119.0 });
            self.wait_us(if bit { 60.0 } else { 1.0 });
        }

        fn read_bit(&mut self) -> bool {
            self.low(1.0);
            self.wait_us(13.0); // sampled 14 µs after the falling edge
            let bit = self.line();
            self.wait_us(50.0);
            bit
        }

        fn write(&mut self, byte: u8) {
            for i in 0..8 {
                self.write_bit(byte >> i & 1 != 0);
            }
        }

        fn read(&mut self) -> u8 {
            (0..8).fold(0, |acc, i| acc | u8::from(self.read_bit()) << i)
        }

        /// Search ROM (Maxim AN187): all ROM codes on the line.
        fn search(&mut self) -> Vec<[u8; 8]> {
            let mut found = Vec::new();
            let mut last_discrepancy = 0;
            let mut rom = [0u8; 8];
            loop {
                assert!(self.reset());
                self.write(0xF0);
                let mut discrepancy = 0;
                for bit in 1..=64usize {
                    let (b, c) = (self.read_bit(), self.read_bit());
                    assert!(!(b && c), "no device answered");
                    let dir = if b != c {
                        b
                    } else {
                        let prev = rom[(bit - 1) / 8] >> ((bit - 1) % 8) & 1 != 0;
                        let dir = if bit < last_discrepancy { prev } else { bit == last_discrepancy };
                        if !dir {
                            discrepancy = bit;
                        }
                        dir
                    };
                    let (byte, mask) = ((bit - 1) / 8, 1 << ((bit - 1) % 8));
                    if dir {
                        rom[byte] |= mask
                    } else {
                        rom[byte] &= !mask
                    }
                    self.write_bit(dir);
                }
                found.push(rom);
                last_discrepancy = discrepancy;
                if last_discrepancy == 0 {
                    return found;
                }
            }
        }
    }

    #[test]
    fn search_finds_two_sensors_on_one_line() {
        let mut m = Master::new(2);
        let mut roms: Vec<[u8; 8]> = m.bus.sensors.iter().map(Ds18b20::rom).collect();
        let mut found = m.search();
        roms.sort();
        found.sort();
        assert_eq!(found, roms);
        for rom in &found {
            assert_eq!(crc8(&rom[..7]), rom[7]);
        }
        // each answers only to its own ROM code
        m.bus.sensors[1].set_celsius(-10.125);
        for (n, rom) in m.bus.sensors.iter().map(Ds18b20::rom).collect::<Vec<_>>().into_iter().enumerate() {
            assert!(m.reset());
            m.write(0x55);
            rom.iter().for_each(|&b| m.write(b));
            m.write(0x44);
            m.wait_us(760_000.0);
            assert!(m.reset());
            m.write(0x55);
            rom.iter().for_each(|&b| m.write(b));
            m.write(0xBE);
            let pad: Vec<u8> = (0..9).map(|_| m.read()).collect();
            assert_eq!(crc8(&pad), 0, "sensor {n}: {pad:02x?}");
            let t = i16::from_le_bytes([pad[0], pad[1]]);
            assert_eq!(t, if n == 1 { -162 } else { 21 * 16 }, "sensor {n}");
        }
    }

    #[test]
    fn no_presence_without_a_reset_pulse_long_enough() {
        let mut m = Master::new(1);
        m.low(120.0); // a slot, not a reset
        m.wait_us(30.0);
        assert!(m.line(), "no presence pulse");
        assert!(m.reset());
    }
}
