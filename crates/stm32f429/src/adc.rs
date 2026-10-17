//! ADC1–3 and the DAC, with the analog levels of the pins.
//!
//! Conversions take (sampling time + resolution) ADC clocks, after a start
//! latency of one ADC clock and four APB2 clocks; regular
//! (single, scan, continuous) and injected groups, EOC/JEOC/OVR/STRT flags,
//! the analog watchdog and the interrupt are modelled, external triggers and
//! DMA are not. Readings of the internal channels, of open pins and the
//! transfer function of the buffered DAC were measured on the board
//! (HwCheck `adc`).

use std::cell::RefCell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::dma::{DmaLine, SharedDma};
use crate::gpio::{NUM_PORTS, PinMode, SharedGpio, port};
use crate::rcc::SharedClocks;

/// Analog supply, from VREFINT (1.21 V) reading 1495.
pub const VDDA: f32 = 3.31;
const VREFINT_COUNTS: u32 = 1495;
const TEMP_COUNTS: u32 = 952;
const VBAT_COUNTS: u32 = 1021;
/// Open pins read about this, with this much noise (counts).
const OPEN_COUNTS: u32 = 1150;
const OPEN_NOISE: u32 = 100;

/// Voltages applied to pins from outside (`None`: open) and the DAC outputs.
pub struct AnalogState {
    pins: [[Option<f32>; 16]; NUM_PORTS],
    /// Output voltages of DAC channels 1 (PA4) and 2 (PA5) while enabled.
    dac: [Option<f32>; 2],
    noise: u32,
}

impl Default for AnalogState {
    fn default() -> Self {
        Self { pins: [[None; 16]; NUM_PORTS], dac: [None; 2], noise: 0x1234_5678 }
    }
}

impl AnalogState {
    /// Applies `volts` to a pin, or leaves it open (`None`).
    pub fn set_pin(&mut self, port: usize, pin: u32, volts: Option<f32>) {
        self.pins[port][pin as usize] = volts;
    }

    pub fn pin(&self, port: usize, pin: u32) -> Option<f32> {
        self.pins[port][pin as usize]
    }

    /// Output voltage of DAC channel `ch` (0 or 1) if it is enabled.
    pub fn dac_output(&self, ch: usize) -> Option<f32> {
        self.dac[ch]
    }

    /// Counts (12 bit) a conversion of the pin gives.
    fn counts(&mut self, port: usize, pin: u32) -> u32 {
        let dac = match (port, pin) {
            (0, 4) => self.dac[0],
            (0, 5) => self.dac[1],
            _ => None,
        };
        match dac.or(self.pins[port][pin as usize]) {
            Some(v) => ((v / VDDA).clamp(0.0, 1.0) * 4095.0).round() as u32,
            None => {
                // xorshift: open inputs pick up noise
                self.noise ^= self.noise << 13;
                self.noise ^= self.noise >> 17;
                self.noise ^= self.noise << 5;
                OPEN_COUNTS - OPEN_NOISE + self.noise % (2 * OPEN_NOISE)
            }
        }
    }
}

pub type SharedAnalog = Rc<RefCell<AnalogState>>;

const SR_AWD: u32 = 1 << 0;
const SR_EOC: u32 = 1 << 1;
const SR_JEOC: u32 = 1 << 2;
const SR_JSTRT: u32 = 1 << 3;
const SR_STRT: u32 = 1 << 4;
const SR_OVR: u32 = 1 << 5;
const CR1_SCAN: u32 = 1 << 8;
const CR1_EOCIE: u32 = 1 << 5;
const CR1_AWDIE: u32 = 1 << 6;
const CR1_JEOCIE: u32 = 1 << 7;
const CR1_AWDSGL: u32 = 1 << 9;
const CR1_JAWDEN: u32 = 1 << 22;
const CR1_AWDEN: u32 = 1 << 23;
const CR1_OVRIE: u32 = 1 << 26;
const CR2_ADON: u32 = 1 << 0;
const CR2_CONT: u32 = 1 << 1;
const CR2_DMA: u32 = 1 << 8;
const CR2_DDS: u32 = 1 << 9;
const CR2_EOCS: u32 = 1 << 10;
const CR2_ALIGN: u32 = 1 << 11;
const CR2_JSWSTART: u32 = 1 << 22;
const CR2_SWSTART: u32 = 1 << 30;
const CCR_VBATE: u32 = 1 << 22;
const CCR_TSVREFE: u32 = 1 << 23;
const SMP_CYCLES: [u64; 8] = [3, 15, 28, 56, 84, 112, 144, 480];
const ADC_IRQ: u32 = 18;
/// DMA2 streams/channels of ADC1..3.
const DMA_LINES: [DmaLine; 3] =
    [DmaLine(&[(1, 0, 0), (1, 4, 0)]), DmaLine(&[(1, 2, 1), (1, 3, 1)]), DmaLine(&[(1, 0, 2), (1, 1, 2)])];

#[derive(Default)]
struct Unit {
    sr: u32,
    cr1: u32,
    cr2: u32,
    smpr: [u32; 2],
    jofr: [u32; 4],
    htr: u32,
    ltr: u32,
    sqr: [u32; 3],
    jsqr: u32,
    jdr: [u32; 4],
    dr: u32,
    /// DMA requests stopped after the last transfer (DDS = 0) until DMA is set again.
    dma_stopped: bool,
    /// Regular group: position in the sequence and end of the running conversion.
    regular: Option<(usize, u64)>,
    injected: Option<(usize, u64)>,
}

impl Unit {
    fn reset() -> Self {
        Self { htr: 0xFFF, ..Self::default() }
    }

    fn regular_len(&self) -> usize {
        if self.cr1 & CR1_SCAN != 0 { ((self.sqr[0] >> 20) & 0xF) as usize + 1 } else { 1 }
    }

    /// Channel of position `i` of the regular sequence.
    fn regular_channel(&self, i: usize) -> u32 {
        let reg = self.sqr[2 - i / 6];
        (reg >> (5 * (i % 6))) & 0x1F
    }

    fn injected_len(&self) -> usize {
        ((self.jsqr >> 20) & 3) as usize + 1
    }

    /// Channel of position `i` of the injected sequence (it ends at JSQ4).
    fn injected_channel(&self, i: usize) -> u32 {
        let slot = 4 - self.injected_len() + i;
        (self.jsqr >> (5 * slot)) & 0x1F
    }

    fn resolution_bits(&self) -> u32 {
        12 - 2 * ((self.cr1 >> 24) & 3)
    }

    fn conversion_adc_clocks(&self, ch: u32) -> u64 {
        let smp = if ch >= 10 { self.smpr[0] >> (3 * (ch - 10)) } else { self.smpr[1] >> (3 * ch) };
        SMP_CYCLES[(smp & 7) as usize] + u64::from(self.resolution_bits())
    }

    /// Result as stored in a data register (resolution, alignment).
    fn format(&self, counts: u32, offset: u32) -> u32 {
        let bits = self.resolution_bits();
        let v = (counts >> (12 - bits)) as i32 - (offset >> (12 - bits)) as i32;
        if self.cr2 & CR2_ALIGN == 0 {
            return (v.max(-(1 << bits)) as u32) & 0xFFFF;
        }
        // left aligned (6-bit results in the low byte)
        let shift = if bits == 6 { 2 } else { 16 - bits };
        ((v as u32) << shift) & 0xFFFF
    }

    fn watchdog(&mut self, ch: u32, counts: u32, injected: bool) {
        let enabled = if injected { self.cr1 & CR1_JAWDEN != 0 } else { self.cr1 & CR1_AWDEN != 0 };
        let single_ok = self.cr1 & CR1_AWDSGL == 0 || self.cr1 & 0x1F == ch;
        let v = counts >> (12 - self.resolution_bits());
        if enabled && single_ok && (v > self.htr || v < self.ltr) {
            self.sr |= SR_AWD;
        }
    }

    fn irq(&self) -> bool {
        (self.cr1 & CR1_EOCIE != 0 && self.sr & SR_EOC != 0)
            || (self.cr1 & CR1_JEOCIE != 0 && self.sr & SR_JEOC != 0)
            || (self.cr1 & CR1_AWDIE != 0 && self.sr & SR_AWD != 0)
            || (self.cr1 & CR1_OVRIE != 0 && self.sr & SR_OVR != 0)
    }
}

/// ADC1, ADC2, ADC3 and their common registers (one 1 KiB window).
pub struct Adc {
    units: [Unit; 3],
    ccr: u32,
    clocks: SharedClocks,
    gpio: SharedGpio,
    analog: SharedAnalog,
    dma: SharedDma,
    warned: bool,
}

impl Adc {
    pub fn new(clocks: SharedClocks, gpio: SharedGpio, analog: SharedAnalog, dma: SharedDma) -> Self {
        let units = [Unit::reset(), Unit::reset(), Unit::reset()];
        Self { units, ccr: 0, clocks, gpio, analog, dma, warned: false }
    }

    fn clock_enabled(&self, unit: usize) -> bool {
        self.clocks.get().apb2enr & (1 << (8 + unit)) != 0
    }

    /// CPU cycles per ADC clock.
    fn cycles_per_adc_clock(&self, ctx: &DeviceCtx) -> u64 {
        let c = self.clocks.get();
        let div = 2 * (u64::from((self.ccr >> 16) & 3) + 1);
        (div * u64::from(ctx.clock.cpu_hz()) / u64::from(c.pclk2.max(1))).max(1)
    }

    /// The pin an external channel of `unit` is connected to.
    fn channel_pin(unit: usize, ch: u32) -> Option<(usize, u32)> {
        let (a, b, c, f) = (port('A'), port('B'), port('C'), port('F'));
        Some(match (unit, ch) {
            (0 | 1, 0..=7) => (a, ch),
            (0 | 1, 8..=9) => (b, ch - 8),
            (0 | 1, 10..=15) => (c, ch - 10),
            (2, 0..=3) => (a, ch),
            (2, 4..=8) => (f, ch + 2),
            (2, 9) => (f, 3),
            (2, 10..=13) => (c, ch - 10),
            (2, 14..=15) => (f, ch - 10),
            _ => return None,
        })
    }

    /// 12-bit result of converting channel `ch` of `unit` now.
    fn sample(&self, unit: usize, ch: u32) -> u32 {
        let internal = unit == 0 && self.ccr & CCR_TSVREFE != 0;
        match ch {
            16 if internal => TEMP_COUNTS,
            17 if internal => VREFINT_COUNTS,
            18 if unit == 0 && self.ccr & CCR_VBATE != 0 => VBAT_COUNTS,
            18 if internal => TEMP_COUNTS,
            16..=18 => 1,
            _ => {
                let Some((p, pin)) = Self::channel_pin(unit, ch) else { return 0 };
                // a digital output drives the pin (the analog switch still sees it)
                let g = self.gpio.borrow();
                let ps = &g.ports[p];
                if ps.mode(pin) == PinMode::Output {
                    return if ps.level(pin) { 4095 } else { 0 };
                }
                drop(g);
                self.analog.borrow_mut().counts(p, pin)
            }
        }
    }

    /// Completes the conversions that end by `now`.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        for (u, unit) in self.units.iter_mut().enumerate() {
            if DMA_LINES[u].take_end(&self.dma) && unit.cr2 & CR2_DDS == 0 {
                unit.dma_stopped = true;
            }
        }
        let now = ctx.now();
        let per_clock = self.cycles_per_adc_clock(ctx);
        for u in 0..3 {
            // regular group; a long continuous run is shortened to its last sequences
            let mut guard = 0;
            while let Some((i, end)) = self.units[u].regular
                && end <= now
            {
                guard += 1;
                let ch = self.units[u].regular_channel(i);
                let counts = self.sample(u, ch);
                let unit = &mut self.units[u];
                // with DMA every conversion is a result to collect
                let dma = unit.cr2 & CR2_DMA != 0 && !unit.dma_stopped;
                let eocs = unit.cr2 & CR2_EOCS != 0 || dma;
                let last = i + 1 >= unit.regular_len();
                if eocs || last {
                    if eocs && unit.sr & SR_EOC != 0 {
                        // data lost: the ADC stops until OVR is cleared
                        unit.sr |= SR_OVR;
                        unit.regular = None;
                        break;
                    }
                    unit.sr |= SR_EOC;
                }
                unit.dr = unit.format(counts, 0);
                unit.watchdog(ch, counts, false);
                let next = if !last {
                    Some(i + 1)
                } else if unit.cr2 & CR2_CONT != 0 {
                    Some(0)
                } else {
                    None
                };
                let mut start = end;
                if guard > 64 && unit.cr2 & CR2_CONT != 0 {
                    // skip whole sequences that nobody could observe
                    let seq: u64 =
                        (0..unit.regular_len()).map(|k| unit.conversion_adc_clocks(unit.regular_channel(k))).sum();
                    let seq = seq * per_clock;
                    start += (now - start) / seq * seq;
                }
                unit.regular =
                    next.map(|k| (k, start + unit.conversion_adc_clocks(unit.regular_channel(k)) * per_clock));
                if dma {
                    break; // the DMA reads DR before the next result
                }
            }
            while let Some((i, end)) = self.units[u].injected
                && end <= now
            {
                let ch = self.units[u].injected_channel(i);
                let counts = self.sample(u, ch);
                let unit = &mut self.units[u];
                unit.jdr[i] = unit.format(counts, unit.jofr[i] & 0xFFF);
                unit.watchdog(ch, counts, true);
                unit.injected = if i + 1 < unit.injected_len() {
                    let ch = unit.injected_channel(i + 1);
                    Some((i + 1, end + unit.conversion_adc_clocks(ch) * per_clock))
                } else {
                    unit.sr |= SR_JEOC;
                    None
                };
            }
        }
        self.sync_irq(ctx);
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        for (u, unit) in self.units.iter().enumerate() {
            ctx.set_irq_source(ADC_IRQ, u as u8, unit.irq());
            DMA_LINES[u].set(&self.dma, unit.cr2 & CR2_DMA != 0 && !unit.dma_stopped && unit.sr & SR_EOC != 0);
        }
    }

    /// CPU cycles from a software start until the first conversion begins.
    fn start_latency(&self, ctx: &DeviceCtx) -> u64 {
        let c = self.clocks.get();
        self.cycles_per_adc_clock(ctx) + 4 * u64::from(c.hclk / c.pclk2.max(1))
    }

    fn start_regular(&mut self, u: usize, ctx: &DeviceCtx) {
        let (per_clock, latency) = (self.cycles_per_adc_clock(ctx), self.start_latency(ctx));
        let unit = &mut self.units[u];
        let ch = unit.regular_channel(0);
        unit.sr |= SR_STRT;
        unit.regular = Some((0, ctx.now() + latency + unit.conversion_adc_clocks(ch) * per_clock));
    }

    fn start_injected(&mut self, u: usize, ctx: &DeviceCtx) {
        let (per_clock, latency) = (self.cycles_per_adc_clock(ctx), self.start_latency(ctx));
        let unit = &mut self.units[u];
        let ch = unit.injected_channel(0);
        unit.sr |= SR_JSTRT;
        unit.injected = Some((0, ctx.now() + latency + unit.conversion_adc_clocks(ch) * per_clock));
    }

    fn reg(&self, u: usize, off: u32) -> u32 {
        let unit = &self.units[u];
        match off {
            0x00 => unit.sr,
            0x04 => unit.cr1,
            0x08 => unit.cr2,
            0x0C => unit.smpr[0],
            0x10 => unit.smpr[1],
            0x14..=0x20 => unit.jofr[(off as usize - 0x14) / 4],
            0x24 => unit.htr,
            0x28 => unit.ltr,
            0x2C..=0x34 => unit.sqr[(off as usize - 0x2C) / 4],
            0x38 => unit.jsqr,
            0x3C..=0x48 => unit.jdr[(off as usize - 0x3C) / 4],
            0x4C => unit.dr,
            _ => 0,
        }
    }
}

impl Peripheral for Adc {
    fn name(&self) -> &str {
        "ADC"
    }

    fn read_stable_until(&self, off: u32, _clock: &Clock) -> u64 {
        let u = (off / 0x100) as usize;
        if u < 3 && matches!(off & 0xFF, 0x00 | 0x3C..=0x4C) {
            let unit = &self.units[u];
            return [unit.regular, unit.injected].into_iter().flatten().map(|(_, end)| end).min().unwrap_or(u64::MAX);
        }
        if off & !3 == 0x300 {
            return self.next_event().unwrap_or(u64::MAX);
        }
        u64::MAX
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        self.advance(ctx);
        let u = (off / 0x100) as usize;
        let reg = off & 0xFC;
        let v = if u < 3 {
            if !self.clock_enabled(u) {
                return 0;
            }
            let v = self.reg(u, reg);
            if reg == 0x4C {
                self.units[u].sr &= !SR_EOC;
                self.sync_irq(ctx);
            }
            v
        } else {
            match reg {
                // CSR: the flags of the three ADCs
                0x00 => self.units.iter().enumerate().fold(0, |acc, (i, unit)| acc | (unit.sr & 0x3F) << (8 * i)),
                0x04 => self.ccr,
                _ => 0,
            }
        };
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        self.advance(ctx);
        let u = (off / 0x100) as usize;
        let reg = off & 0xFC;
        if u == 3 {
            if reg == 0x04 {
                self.ccr = merge_write(self.ccr, off, size, value) & 0x00C3_EF00;
            }
            return;
        }
        if !self.clock_enabled(u) {
            return;
        }
        let v = merge_write(self.reg(u, reg), off, size, value);
        let unit = &mut self.units[u];
        match reg {
            0x00 => unit.sr &= v | !0x3F,
            0x04 => unit.cr1 = v & 0x07C0_FFFF,
            0x08 => {
                if v & CR2_DMA == 0 {
                    unit.dma_stopped = false;
                }
                unit.cr2 = v & 0x3F7F_0F03 & !(CR2_SWSTART | CR2_JSWSTART);
                if unit.cr2 & CR2_ADON == 0 {
                    unit.regular = None;
                    unit.injected = None;
                } else {
                    if v & 0x3030_0000 != 0 && !self.warned {
                        self.warned = true;
                        log::warn!("ADC: external triggers are not emulated");
                    }
                    if v & CR2_SWSTART != 0 && unit.regular.is_none() {
                        self.start_regular(u, ctx);
                    }
                    if v & CR2_JSWSTART != 0 && self.units[u].injected.is_none() {
                        self.start_injected(u, ctx);
                    }
                }
            }
            0x0C => unit.smpr[0] = v & 0x07FF_FFFF,
            0x10 => unit.smpr[1] = v & 0x3FFF_FFFF,
            0x14..=0x20 => unit.jofr[(reg as usize - 0x14) / 4] = v & 0xFFF,
            0x24 => unit.htr = v & 0xFFF,
            0x28 => unit.ltr = v & 0xFFF,
            0x2C => unit.sqr[0] = v & 0x00FF_FFFF,
            0x30 | 0x34 => unit.sqr[(reg as usize - 0x2C) / 4] = v & 0x3FFF_FFFF,
            0x38 => unit.jsqr = v & 0x003F_FFFF,
            _ => {}
        }
        self.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.next_event().is_some_and(|t| t <= ctx.now()) {
            self.advance(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.units.iter().flat_map(|u| [u.regular, u.injected]).flatten().map(|(_, end)| end).min()
    }

    fn reset(&mut self) {
        self.units = [Unit::reset(), Unit::reset(), Unit::reset()];
        self.ccr = 0;
    }
}

/// DAC channels 1 (PA4) and 2 (PA5).
pub struct Dac {
    cr: u32,
    dhr: [u32; 2],
    dor: [u32; 2],
    sr: u32,
    clocks: SharedClocks,
    analog: SharedAnalog,
    warned: bool,
}

impl Dac {
    pub fn new(clocks: SharedClocks, analog: SharedAnalog) -> Self {
        Self { cr: 0, dhr: [0; 2], dor: [0; 2], sr: 0, clocks, analog, warned: false }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().apb1enr & (1 << 29) != 0
    }

    fn ch_cr(&self, ch: usize) -> u32 {
        self.cr >> (16 * ch)
    }

    /// Loads DOR from DHR unless the channel waits for a trigger.
    fn load(&mut self, ch: usize, triggered: bool) {
        if self.ch_cr(ch) & 0x4 == 0 || triggered {
            self.dor[ch] = self.dhr[ch];
        }
    }

    /// Updates the output voltages: the buffered output (BOFF = 0) cannot
    /// reach the rails (measured: ADC counts = 1.0166 * code - 38, 39..4065).
    fn output(&self) {
        let mut a = self.analog.borrow_mut();
        for ch in 0..2 {
            let cr = self.ch_cr(ch);
            a.dac[ch] = (cr & 1 != 0).then(|| {
                let code = self.dor[ch] as f32;
                let counts = if cr & 2 == 0 { (1.0166 * code - 38.0).clamp(39.0, 4065.0) } else { code };
                counts / 4095.0 * VDDA
            });
        }
    }

    fn reg(&self, off: u32) -> u32 {
        match off {
            0x00 => self.cr,
            0x08 | 0x20 => self.dhr[0] | if off == 0x20 { self.dhr[1] << 16 } else { 0 },
            0x0C => self.dhr[0] << 4,
            0x10 => self.dhr[0] >> 4,
            0x14 => self.dhr[1],
            0x18 => self.dhr[1] << 4,
            0x1C => self.dhr[1] >> 4,
            0x24 => (self.dhr[0] << 4) | (self.dhr[1] << 20),
            0x28 => (self.dhr[0] >> 4) | ((self.dhr[1] >> 4) << 8),
            0x2C => self.dor[0],
            0x30 => self.dor[1],
            0x34 => self.sr,
            _ => 0,
        }
    }
}

impl Peripheral for Dac {
    fn name(&self) -> &str {
        "DAC"
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        extract_read(self.reg(off & !3), off, size)
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        let reg = off & !3;
        let v = merge_write(self.reg(reg), off, size, value);
        match reg {
            0x00 => {
                self.cr = v & 0x3FFF_3FFF;
                for ch in 0..2 {
                    let cr = self.ch_cr(ch);
                    let software = (cr >> 3) & 7 == 7;
                    let unsupported = cr & 0x4 != 0 && !software || (cr >> 6) & 3 != 0 || cr & 0x1000 != 0;
                    if unsupported && !self.warned {
                        self.warned = true;
                        log::warn!("DAC: hardware triggers, wave generation and DMA are not emulated");
                    }
                }
            }
            0x04 => {
                for ch in 0..2 {
                    if v & (1 << ch) != 0 && (self.ch_cr(ch) >> 3) & 7 == 7 {
                        self.load(ch, true);
                    }
                }
            }
            0x08 => self.dhr[0] = v & 0xFFF,
            0x0C => self.dhr[0] = (v >> 4) & 0xFFF,
            0x10 => self.dhr[0] = (v & 0xFF) << 4,
            0x14 => self.dhr[1] = v & 0xFFF,
            0x18 => self.dhr[1] = (v >> 4) & 0xFFF,
            0x1C => self.dhr[1] = (v & 0xFF) << 4,
            0x20 => (self.dhr[0], self.dhr[1]) = (v & 0xFFF, (v >> 16) & 0xFFF),
            0x24 => (self.dhr[0], self.dhr[1]) = ((v >> 4) & 0xFFF, (v >> 20) & 0xFFF),
            0x28 => (self.dhr[0], self.dhr[1]) = ((v & 0xFF) << 4, ((v >> 8) & 0xFF) << 4),
            0x34 => self.sr &= !v,
            _ => {}
        }
        if (0x08..=0x28).contains(&reg) {
            self.load(0, false);
            self.load(1, false);
        }
        self.output();
    }

    fn reset(&mut self) {
        (self.cr, self.dhr, self.dor, self.sr) = (0, [0; 2], [0; 2], 0);
        self.output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rcc::Clocks;
    use emu_core::nvic::Nvic;

    fn adc() -> (Adc, SharedAnalog) {
        let clocks = SharedClocks::default();
        let hz = 180_000_000;
        clocks.set(Clocks { hclk: hz, pclk2: hz / 2, apb2enr: 1 << 8, ..Clocks::default() });
        let analog = SharedAnalog::default();
        (Adc::new(clocks, SharedGpio::default(), analog.clone(), SharedDma::default()), analog)
    }

    #[test]
    fn scan_of_a_pin_and_vrefint() {
        let (mut adc, analog) = adc();
        analog.borrow_mut().set_pin(port('C'), 3, Some(VDDA / 2.0));
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        adc.write(&mut ctx, 0x304, 4, CCR_TSVREFE); // PCLK2 / 2 = 45 MHz: 4 cycles per ADC clock
        adc.write(&mut ctx, 0x04, 4, CR1_SCAN);
        adc.write(&mut ctx, 0x2C, 4, 1 << 20); // two conversions
        adc.write(&mut ctx, 0x34, 4, 13 | (17 << 5)); // PC3 (IN13), VREFINT
        adc.write(&mut ctx, 0x08, 4, CR2_ADON | CR2_EOCS);
        adc.write(&mut ctx, 0x08, 4, CR2_ADON | CR2_EOCS | CR2_SWSTART);
        assert_eq!(adc.read(&mut ctx, 0x00, 4), SR_STRT);
        // start latency 1 ADC clock + 4 APB2 clocks, conversion 3 + 12 ADC clocks
        let first = 4 + 8 + 15 * 4;
        assert_eq!(adc.next_event(), Some(first));
        ctx.clock.advance(first);
        assert_eq!(adc.read(&mut ctx, 0x00, 4), SR_STRT | SR_EOC);
        assert_eq!(adc.read(&mut ctx, 0x4C, 4), 2048);
        assert_eq!(adc.read(&mut ctx, 0x00, 4), SR_STRT, "reading DR clears EOC");
        ctx.clock.advance(15 * 4);
        assert_eq!(adc.read(&mut ctx, 0x4C, 4), VREFINT_COUNTS);
        assert_eq!(adc.next_event(), None);
    }
}
