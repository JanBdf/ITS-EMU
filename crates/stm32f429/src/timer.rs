//! Timers (TIM1–TIM14).
//!
//! The counter is evaluated lazily from virtual time, as a phase in its cycle:
//! up-counting (CNT = phase), down-counting (CNT = ARR − phase) and
//! center-aligned (up to ARR, then down: a cycle of 2·ARR ticks with update
//! events at both ends; CMS selects the directions that set the CC flags). Supported: prescaler and
//! auto-reload and compare values (with preload), update events (overflow, `EGR`), the
//! repetition counter of TIM1/TIM8, output compare flags and interrupts, PWM
//! duty cycles, `CNT` reads/writes, input capture of edges that drive the
//! channel pins from outside (buttons, analog inputs; with prescaler,
//! polarity and overcapture), and the output compare channels driving their
//! pins (frozen, active/inactive/toggle on match, forced, PWM 1/2; MOE of
//! TIM1/TIM8), encoder mode (SMS 1–3: TI1 and/or TI2 edges count up/down by the
//! other input's level, as measured with HwCheck `enc`), the slave modes reset,
//! gated, trigger and external clock 1 with TI1F_ED/TI1FP1/TI2FP2 as trigger
//! (HwCheck `timslave`). OCxREF changes only at compare matches, counter wraps and
//! forced levels, as measured on the board (HwCheck `timout`): a mode change
//! takes effect at the next of them; PWM compares CNT < CCR counting up and
//! CNT ≤ CCR counting down (HwCheck `timdir`). The other slave modes and the
//! complementary outputs are not modelled (a warning is logged when used).

use std::cell::Cell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::gpio::{PinMode, SharedGpio, port};
use crate::rcc::SharedClocks;

/// Cycle of the last trigger output (TRGO) of TIM2..TIM8 (index
/// `TRGO_TIM2` ...), for the external triggers of the ADCs and the DAC.
pub type SharedTriggers = Rc<Cell<[u64; 7]>>;
pub const TRGO_TIM2: usize = 0;
pub const TRGO_TIM3: usize = 1;
pub const TRGO_TIM4: usize = 2;
pub const TRGO_TIM5: usize = 3;
pub const TRGO_TIM6: usize = 4;
pub const TRGO_TIM7: usize = 5;
pub const TRGO_TIM8: usize = 6;

/// Pins of the four channels of a timer, and their alternate function.
pub struct ChannelPins {
    pub af: u32,
    pub pins: [&'static [(char, u32)]; 4],
}

/// Duty cycle (0.0–1.0) of each output channel in PWM mode, `None` otherwise.
pub type SharedPwm = Rc<Cell<[Option<f32>; 4]>>;

const PS_PER_S: u128 = 1_000_000_000_000;

const CR1_CEN: u32 = 1 << 0;
const CR1_UDIS: u32 = 1 << 1;
const CR1_URS: u32 = 1 << 2;
const CR1_OPM: u32 = 1 << 3;
const CR1_DIR: u32 = 1 << 4;
const CR1_ARPE: u32 = 1 << 7;
const SR_UIF: u32 = 1 << 0;
const SR_TIF: u32 = 1 << 6;
const DIER_UIE: u32 = 1 << 0;
const BDTR_MOE: u32 = 1 << 15;
/// Timer clocks from an output edge until its own channel input captures it
/// (measured: toggle on CCR1 = 500, CH2 captures 512).
const LOOPBACK_CLOCKS: u64 = 12;

/// Which APB bus clocks the timer (selects the kernel clock and enable register).
#[derive(Debug, Clone, Copy)]
pub enum ApbBus {
    Apb1,
    Apb2,
}

pub struct GpTimer {
    name: &'static str,
    irq: u32,
    /// Source index of the timer on its (possibly shared) interrupt lines.
    irq_src: u8,
    /// Advanced-control timers (TIM1/TIM8): interrupt line of the
    /// capture/compare events (the update event has its own).
    cc_irq: Option<u32>,
    wide: bool,
    /// Number of capture/compare channels.
    channels: usize,
    /// Writable bits of CR1 and DIER (measured per timer type).
    cr1_mask: u32,
    dier_mask: u32,
    bus: ApbBus,
    enable_bit: u32,
    clocks: SharedClocks,
    pwm: SharedPwm,
    /// Inputs for capture: the pins and the GPIO bank.
    inputs: Option<(&'static ChannelPins, SharedGpio)>,
    /// Pins watched for capture edges (port, pin).
    watched: Vec<(usize, u32)>,
    /// Edges counted by the input prescalers.
    ic_events: [u32; 4],
    /// Trigger output to the ADCs.
    trgo: Option<(SharedTriggers, usize)>,
    /// Output compare reference (OCxREF) and the level driven on the pins.
    oc_ref: [bool; 4],
    oc_out: [Option<bool>; 4],
    /// Encoder mode: pins of TI1/TI2 whose edges are logged, and the inputs'
    /// levels (after the polarity CCxP).
    enc_watched: Vec<(usize, u32)>,
    enc_level: [bool; 2],
    /// Gated slave mode: the trigger input lets the counter count.
    gate: bool,

    cr1: u32,
    cr2: u32,
    smcr: u32,
    dier: u32,
    sr: u32,
    ccmr: [u32; 2],
    ccer: u32,
    psc: u32,
    psc_active: u32,
    arr: u32,
    arr_active: u32,
    rcr: u32,
    /// Overflows left until the next update event (repetition counter).
    rep_left: u32,
    /// CCR as written (the preload register) and as compared.
    ccr: [u32; 4],
    ccr_active: [u32; 4],
    bdtr: u32,
    dcr: u32,
    or: u32,

    /// Counter value at `base_ps`.
    cnt_base: u32,
    base_ps: u64,
    /// Ticks after `base_ps` up to which compare matches have been flagged.
    checked: u128,
    /// Next update or compare interrupt in CPU cycles.
    next_event: Option<u64>,
    warned: bool,
    /// Warned about an access while the clock is disabled (once: firmware
    /// may keep writing, and a log line per access slows emulation down).
    warned_clock: bool,
}

impl GpTimer {
    pub fn new(name: &'static str, irq: u32, wide: bool, bus: ApbBus, enable_bit: u32, clocks: SharedClocks) -> Self {
        let mut t = Self {
            name,
            irq,
            irq_src: 0,
            cc_irq: None,
            wide,
            channels: 4,
            cr1_mask: 0x3FF,
            dier_mask: 0x5F5F,
            bus,
            enable_bit,
            clocks,
            pwm: SharedPwm::default(),
            inputs: None,
            watched: Vec::new(),
            ic_events: [0; 4],
            trgo: None,
            oc_ref: [false; 4],
            oc_out: [None; 4],
            enc_watched: Vec::new(),
            enc_level: [false; 2],
            gate: false,
            cr1: 0,
            cr2: 0,
            smcr: 0,
            dier: 0,
            sr: 0,
            ccmr: [0; 2],
            ccer: 0,
            psc: 0,
            psc_active: 0,
            arr: 0,
            arr_active: 0,
            rcr: 0,
            rep_left: 0,
            ccr: [0; 4],
            ccr_active: [0; 4],
            bdtr: 0,
            dcr: 0,
            or: 0,
            cnt_base: 0,
            base_ps: 0,
            checked: 0,
            next_event: None,
            warned: false,
            warned_clock: false,
        };
        t.reset_regs();
        t
    }

    /// Advanced-control timer whose capture/compare events go to `cc_irq`.
    pub fn advanced(mut self, cc_irq: u32) -> Self {
        self.cc_irq = Some(cc_irq);
        self.dier_mask = 0xFFFF;
        self
    }

    /// Basic (0 channels) or small general-purpose timer (TIM9..14).
    pub fn with_channels(mut self, n: usize) -> Self {
        self.channels = n;
        (self.cr1_mask, self.dier_mask) = match n {
            0 => (0x8F, 0x4141),
            1 => (0x38F, 0x03),
            2 => (0x38F, 0x47),
            _ => (0x3FF, 0x5F5F),
        };
        self
    }

    /// Trigger output (master mode "update") for the ADCs and the DAC.
    pub fn with_trgo(mut self, triggers: SharedTriggers, index: usize) -> Self {
        self.trgo = Some((triggers, index));
        self
    }

    /// TRGO is the update event (CR2.MMS = 010) and someone may listen.
    fn trgo_on_update(&self) -> bool {
        self.trgo.is_some() && (self.cr2 >> 4) & 7 == 2
    }

    fn emit_trgo(&self, cycle: u64) {
        if let Some((t, i)) = &self.trgo {
            let mut v = t.get();
            v[*i] = cycle;
            t.set(v);
        }
    }

    /// Channel inputs for input capture.
    pub fn with_inputs(mut self, pins: &'static ChannelPins, gpio: SharedGpio) -> Self {
        self.inputs = Some((pins, gpio));
        self
    }

    /// The timer is source `src` of an interrupt line shared with another timer.
    pub fn shared_irq(mut self, src: u8) -> Self {
        self.irq_src = src;
        self
    }

    fn reset_regs(&mut self) {
        let max = self.max();
        (self.cr1, self.cr2, self.smcr, self.dier, self.sr, self.ccer) = (0, 0, 0, 0, 0, 0);
        (self.ccmr, self.ccr, self.ccr_active) = ([0; 2], [0; 4], [0; 4]);
        (self.psc, self.psc_active, self.arr, self.arr_active) = (0, 0, max, max);
        (self.rcr, self.rep_left, self.bdtr, self.dcr, self.or) = (0, 0, 0, 0, 0);
        (self.cnt_base, self.base_ps, self.checked, self.next_event, self.warned) = (0, 0, 0, None, false);
        self.warned_clock = false;
        self.ic_events = [0; 4];
        self.oc_ref = [false; 4];
        self.update_watch();
    }

    /// Capture mode of channel `ch`: the channel whose pin it captures (TI1..TI4).
    fn capture_source(&self, ch: usize) -> Option<usize> {
        if ch >= self.channels || self.ccer & (1 << (4 * ch)) == 0 {
            return None;
        }
        match (self.ccmr[ch / 2] >> (8 * (ch % 2))) & 3 {
            1 => Some(ch),
            2 => Some(ch ^ 1),
            _ => None,
        }
    }

    /// Encoder mode (SMS = 1, 2, 3): the counter counts edges of TI1/TI2.
    fn encoder(&self) -> bool {
        self.channels >= 2 && (1..=3).contains(&(self.smcr & 7))
    }

    /// Slave mode reset/gated/trigger/external clock 1 (SMS = 4..7) with a
    /// trigger input from the channel pins (TS = TI1F_ED, TI1FP1, TI2FP2).
    fn ti_slave(&self) -> Option<(u32, u32)> {
        let (sms, ts) = (self.smcr & 7, (self.smcr >> 4) & 7);
        (self.channels >= 2 && sms >= 4 && (4..=6).contains(&ts)).then_some((sms, ts))
    }

    /// The counter counts input edges instead of the internal clock
    /// (encoder mode, external clock mode 1).
    fn edge_clocked(&self) -> bool {
        self.encoder() || self.ti_slave().is_some_and(|(sms, _)| sms == 7)
    }

    /// Level of the trigger input TRGI (after the polarity of TI1FP1/TI2FP2).
    fn trgi_level(&self) -> bool {
        match self.ti_slave() {
            Some((_, 6)) => self.enc_level[1],
            _ => self.enc_level[0],
        }
    }

    /// Level of input TI1 (`i` = 0) or TI2 from its routed pin, after CCxP.
    fn ti_level(&self, i: usize) -> bool {
        let Some((pins, gpio)) = &self.inputs else { return false };
        let g = gpio.borrow();
        let level = pins.pins[i]
            .iter()
            .find(|&&(p, pin)| {
                let ps = &g.ports[port(p)];
                ps.mode(pin) == PinMode::Alternate && ps.af(pin) == pins.af
            })
            .is_some_and(|&(p, pin)| g.level(port(p), pin));
        level ^ (self.ccer & (2 << (4 * i)) != 0)
    }

    /// Takes the logged edges of TI1/TI2: counts them in encoder mode, acts
    /// on trigger edges (at their time) in the slave modes.
    fn take_input_edges(&mut self, ctx: &mut DeviceCtx, now_ps: u64) {
        let Some((pins, gpio)) = &self.inputs else { return };
        if self.enc_watched.is_empty() {
            return;
        }
        let mut edges = Vec::new();
        {
            let mut g = gpio.borrow_mut();
            for &(p, pin) in &self.enc_watched {
                let routed = {
                    let ps = &g.ports[p];
                    ps.mode(pin) == PinMode::Alternate && ps.af(pin) == pins.af
                };
                for (_, level, cycle, _) in g.take_edges(p, 1 << pin) {
                    if routed {
                        let i = usize::from(!pins.pins[0].iter().any(|&(pp, pn)| port(pp) == p && pn == pin));
                        edges.push((cycle, i, level));
                    }
                }
            }
        }
        edges.sort_by_key(|&(cycle, _, _)| cycle);
        if let Some((sms, ts)) = self.ti_slave() {
            for (cycle, i, raw) in edges {
                let level = raw ^ (self.ccer & (2 << (4 * i)) != 0);
                if level == self.enc_level[i] {
                    continue;
                }
                self.enc_level[i] = level;
                // TI1F_ED: every edge of TI1; TI1FP1/TI2FP2: rising after the polarity
                let input = if ts == 6 { 1 } else { 0 };
                if i != input {
                    continue;
                }
                let trigger = ts == 4 || level;
                let t = if cycle == 0 { now_ps } else { ctx.clock.ps_at_cycle(cycle).min(now_ps) };
                match sms {
                    7 => {
                        if trigger {
                            self.sr |= SR_TIF;
                            if self.running() {
                                self.count_edge(true);
                            }
                        }
                    }
                    4 if trigger => {
                        self.sr |= SR_TIF;
                        self.advance_to(ctx, t);
                        let flag = self.cr1 & CR1_URS == 0;
                        self.reinit(ctx, t, flag);
                    }
                    5 => {
                        // gated: counts while TRGI is high; TIF on both edges
                        self.sr |= SR_TIF;
                        self.advance_to(ctx, t);
                        let phase = self.phase_now(t).min(u128::from(u32::MAX)) as u32;
                        self.gate = self.trgi_level();
                        self.rebase(t, phase);
                    }
                    6 if trigger => {
                        self.sr |= SR_TIF;
                        self.advance_to(ctx, t);
                        if !self.running() {
                            self.cr1 |= CR1_CEN;
                            self.base_ps = t;
                            self.checked = 0;
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        for (_, i, level) in edges {
            let level = level ^ (self.ccer & (2 << (4 * i)) != 0);
            if level == self.enc_level[i] {
                continue;
            }
            self.enc_level[i] = level;
            let counts = match self.smcr & 7 {
                1 => i == 0,
                2 => i == 1,
                _ => true,
            };
            if !counts || !self.running() {
                continue;
            }
            let [a, b] = self.enc_level;
            let up = if i == 0 { a != b } else { a == b };
            self.count_edge(up);
        }
    }

    /// One count of an edge-clocked counter (encoder, external clock).
    fn count_edge(&mut self, up: bool) {
        let max = self.arr_active;
        if up {
            self.cr1 &= !CR1_DIR;
            if self.cnt_base >= max {
                self.cnt_base = 0;
                self.encoder_update();
            } else {
                self.cnt_base += 1;
            }
        } else {
            self.cr1 |= CR1_DIR;
            if self.cnt_base == 0 {
                self.cnt_base = max;
                self.encoder_update();
            } else {
                self.cnt_base -= 1;
            }
        }
    }

    /// Re-initialises counter and prescaler at `t_ps` (UG, reset mode): an
    /// update event (`flag`: with UIF).
    fn reinit(&mut self, ctx: &DeviceCtx, t_ps: u64, flag: bool) {
        self.update_event(flag);
        self.rebase(t_ps, 0);
        self.base_ps = t_ps;
        for ch in 0..self.channels {
            self.oc_wrap(ch, 0);
        }
        if self.trgo_on_update() {
            self.emit_trgo(ctx.clock.cycle_at_ps(t_ps)); // an update event for TRGO, too
        }
    }

    /// Over- or underflow in encoder mode.
    fn encoder_update(&mut self) {
        if self.cr1 & CR1_UDIS == 0 {
            self.update_event(true);
        }
    }

    /// Tells the GPIO bank which pins to report edges of.
    fn update_watch(&mut self) {
        let encoder = self.encoder() || self.ti_slave().is_some();
        let levels = [self.ti_level(0), self.ti_level(1)];
        let Some((pins, gpio)) = &self.inputs else { return };
        let mut g = gpio.borrow_mut();
        for &(p, pin) in &self.enc_watched {
            g.ports[p].log_watch &= !(1 << pin);
            g.take_edges(p, 1 << pin);
        }
        self.enc_watched.clear();
        if encoder {
            for &(p, pin) in pins.pins[0].iter().chain(pins.pins[1]) {
                g.ports[port(p)].log_watch |= 1 << pin;
                self.enc_watched.push((port(p), pin));
            }
            self.enc_level = levels;
            self.gate = self.trgi_level();
        }
        for &(p, pin) in &self.watched {
            let ps = &mut g.ports[p];
            ps.cap_watch &= !(1 << pin);
            ps.cap_rise &= !(1 << pin);
            ps.cap_fall &= !(1 << pin);
        }
        self.watched.clear();
        for ch in 0..4 {
            let Some(src) = self.capture_source(ch) else { continue };
            for &(p, pin) in pins.pins[src] {
                g.ports[port(p)].cap_watch |= 1 << pin;
                self.watched.push((port(p), pin));
            }
        }
    }

    /// Captures the counter on the edges the watched pins had since the last call.
    fn take_captures(&mut self, now_ps: u64) {
        let Some((pins, gpio)) = &self.inputs else { return };
        if self.watched.is_empty() {
            return;
        }
        let mut g = gpio.borrow_mut();
        // edges per channel input (rising, falling, time if known), from pins routed to the timer
        let mut edges = [(false, false, 0u64); 4];
        for (src, list) in pins.pins.iter().enumerate() {
            for &(p, pin) in *list {
                let ps = &mut g.ports[port(p)];
                let bit = 1u16 << pin;
                let routed = ps.mode(pin) == PinMode::Alternate && ps.af(pin) == pins.af;
                if routed && (ps.cap_rise | ps.cap_fall) & bit != 0 {
                    edges[src].0 |= ps.cap_rise & bit != 0;
                    edges[src].1 |= ps.cap_fall & bit != 0;
                    edges[src].2 = ps.cap_ps[pin as usize];
                }
                ps.cap_rise &= !bit;
                ps.cap_fall &= !bit;
            }
        }
        drop(g);
        let delay_ps = (u128::from(LOOPBACK_CLOCKS) * PS_PER_S / u128::from(self.kernel_hz().max(1))) as u64;
        for ch in 0..4 {
            let Some(src) = self.capture_source(ch) else { continue };
            let (rise, fall, edge_ps) = edges[src];
            let cnt = self.cnt(if edge_ps == 0 { now_ps } else { edge_ps + delay_ps });
            let (p, np) = (self.ccer & (2 << (4 * ch)) != 0, self.ccer & (8 << (4 * ch)) != 0);
            let hit = match (p, np) {
                (false, false) => rise,
                (true, false) => fall,
                _ => rise || fall,
            };
            if !hit {
                continue;
            }
            let psc = (self.ccmr[ch / 2] >> (8 * (ch % 2) + 2)) & 3;
            self.ic_events[ch] += 1;
            if self.ic_events[ch] < 1 << psc {
                continue;
            }
            self.ic_events[ch] = 0;
            if self.sr & (2 << ch) != 0 {
                self.sr |= 0x200 << ch; // CCxOF: overcapture
            }
            self.sr |= 2 << ch;
            self.ccr[ch] = cnt;
        }
    }

    /// Output compare mode (OCxM) of channel `ch`.
    fn oc_mode(&self, ch: usize) -> u32 {
        (self.ccmr[ch / 2] >> (8 * (ch % 2) + 4)) & 7
    }

    /// Level the channel drives on its pins: an enabled output compare
    /// channel (with MOE on TIM1/TIM8), OCxREF with the polarity CCxP.
    fn oc_level(&self, ch: usize) -> Option<bool> {
        let enabled = self.compares(ch)
            && self.ccer & (1 << (4 * ch)) != 0
            && (self.cc_irq.is_none() || self.bdtr & BDTR_MOE != 0);
        enabled.then(|| self.oc_ref[ch] ^ (self.ccer & (2 << (4 * ch)) != 0))
    }

    /// PWM mode 1 is active while CNT < CCR counting up, CNT ≤ CCR counting down.
    fn pwm_active(&self, ch: usize, cnt: u32, down: bool) -> bool {
        let c = self.ccr_active[ch];
        if down { cnt <= c } else { cnt < c }
    }

    /// The counter wrapped or turned (phase `q`): PWM channels compare again.
    fn oc_wrap(&mut self, ch: usize, q: u128) -> bool {
        let before = self.oc_ref[ch];
        let (cnt, down) = self.cnt_at(q);
        match self.oc_mode(ch) {
            6 => self.oc_ref[ch] = self.pwm_active(ch, cnt, down),
            7 => self.oc_ref[ch] = !self.pwm_active(ch, cnt, down),
            _ => {}
        }
        self.oc_ref[ch] != before
    }

    /// The counter reached CCR (at phase `q`).
    fn oc_match(&mut self, ch: usize, q: u128) -> bool {
        let before = self.oc_ref[ch];
        match self.oc_mode(ch) {
            1 => self.oc_ref[ch] = true,
            2 => self.oc_ref[ch] = false,
            3 => self.oc_ref[ch] = !before,
            6 | 7 => {
                self.oc_wrap(ch, q);
            }
            _ => {}
        }
        self.oc_ref[ch] != before
    }

    /// Counter value of the output match of `ch` (0 matches at the wrap, a
    /// CCR above ARR never), as a position in a period: 1..=ARR+1.
    fn oc_match_pos(&self, ch: usize) -> Option<u128> {
        let ccr = self.ccr_active[ch];
        (self.compares(ch) && ccr <= self.arr_active).then(|| if ccr == 0 { self.period() } else { u128::from(ccr) })
    }

    /// Updates OCxREF for the counter going from position `from` (exclusive)
    /// to `to` (inclusive) of a period, `to` = ARR + 1 being the wrap to 0.
    /// Returns per channel the position of its last change.
    fn oc_span(&mut self, from: u128, to: u128) -> [Option<u128>; 4] {
        let mut last = [None; 4];
        let wrap = self.period();
        // the turn at the top (center-aligned) also re-evaluates PWM
        let top = (self.center() != 0 && !self.edge_clocked()).then(|| u128::from(self.arr_active));
        for (ch, last) in last.iter_mut().enumerate().take(self.channels) {
            let mut events: Vec<(u128, u8)> = self
                .match_phases(ch, true)
                .into_iter()
                .flatten()
                .filter(|&m| from < m && m <= to)
                .map(|m| (m, 1))
                .collect();
            for q in [top, Some(wrap)].into_iter().flatten() {
                if from < q && q <= to {
                    events.push((q, 0)); // the wrap first, then a match at the same phase
                }
            }
            events.sort();
            for (q, kind) in events {
                let changed = if kind == 0 { self.oc_wrap(ch, q) } else { self.oc_match(ch, q) };
                if changed {
                    *last = Some(q);
                }
            }
        }
        last
    }

    /// `n` complete periods (each with its match and the wrap at the end).
    fn oc_periods(&mut self, n: u128) -> [bool; 4] {
        let mut changed = [false; 4];
        let period = self.period();
        for (ch, changed) in changed.iter_mut().enumerate().take(self.channels) {
            let before = self.oc_ref[ch];
            let matches = self.match_phases(ch, true).into_iter().flatten().count() as u128;
            match self.oc_mode(ch) {
                3 if (matches * n) % 2 == 1 => self.oc_ref[ch] = !before,
                1 | 2 if matches > 0 => {
                    self.oc_match(ch, period);
                }
                6 | 7 => {
                    self.oc_wrap(ch, period);
                }
                _ => {}
            }
            *changed = self.oc_ref[ch] != before;
        }
        changed
    }

    /// Drives the pins of the channels whose level changed; `edge_ps` is
    /// the time of the change.
    fn push_outputs(&mut self, clock: &Clock, edge_ps: [u64; 4]) {
        let Some((pins, gpio)) = &self.inputs else { return };
        for (ch, &edge) in edge_ps.iter().enumerate().take(self.channels) {
            let level = self.oc_level(ch);
            if level == self.oc_out[ch] {
                continue;
            }
            self.oc_out[ch] = level;
            let t = edge.max(1);
            let mut g = gpio.borrow_mut();
            for &(p, pin) in pins.pins[ch] {
                g.drive_af(port(p), pin, pins.af, level, clock.cycle_at_ps(t), t);
            }
        }
    }

    /// Time (ps) of position `pos` of the counter, relative to `base`.
    fn pos_ps(&self, pos: u128, base: u128) -> u64 {
        let hz = u128::from(self.kernel_hz().max(1));
        self.base_ps + ((pos - base) * self.tick_ps_num()).div_ceil(hz) as u64
    }

    /// PWM channels follow a change of the comparison (by a write of CCR or
    /// CNT) at once; a mode change alone waits for the next event.
    fn oc_compare_changed(&mut self, ch: usize, before: bool, after: bool) {
        if before != after {
            match self.oc_mode(ch) {
                6 => self.oc_ref[ch] = after,
                7 => self.oc_ref[ch] = !after,
                _ => {}
            }
        }
    }

    /// Handle to the PWM duty cycles of the output channels.
    pub fn pwm(&self) -> SharedPwm {
        self.pwm.clone()
    }

    fn update_pwm(&self) {
        let mut duty = [None; 4];
        if self.running() {
            for (ch, d) in duty.iter_mut().enumerate() {
                let ccmr = self.ccmr[ch / 2] >> (8 * (ch % 2));
                let mode = (ccmr >> 4) & 7;
                let enabled = self.ccer & (1 << (4 * ch)) != 0;
                if !enabled || !matches!(mode, 6 | 7) {
                    continue;
                }
                let (c, arr) = (f64::from(self.ccr_active[ch]), f64::from(self.arr_active));
                let v = if self.center() != 0 {
                    c / arr.max(1.0)
                } else if self.counts_down() {
                    (c + 1.0) / (arr + 1.0)
                } else {
                    c / (arr + 1.0)
                };
                let mut v = v.clamp(0.0, 1.0) as f32;
                if mode == 7 {
                    v = 1.0 - v; // PWM mode 2
                }
                if self.ccer & (2 << (4 * ch)) != 0 {
                    v = 1.0 - v; // active low
                }
                *d = Some(v);
            }
        }
        self.pwm.set(duty);
    }

    fn max(&self) -> u32 {
        if self.wide { u32::MAX } else { 0xFFFF }
    }

    fn kernel_hz(&self) -> u32 {
        let c = self.clocks.get();
        match self.bus {
            ApbBus::Apb1 => c.apb1_timer,
            ApbBus::Apb2 => c.apb2_timer,
        }
    }

    fn clock_enabled(&self) -> bool {
        let c = self.clocks.get();
        let enr = match self.bus {
            ApbBus::Apb1 => c.apb1enr,
            ApbBus::Apb2 => c.apb2enr,
        };
        enr & (1 << self.enable_bit) != 0
    }

    fn running(&self) -> bool {
        self.cr1 & CR1_CEN != 0 && (self.gate || !self.ti_slave().is_some_and(|(sms, _)| sms == 5))
    }

    /// Duration of one counter tick in picoseconds (as a rational: ps * hz).
    fn tick_ps_num(&self) -> u128 {
        PS_PER_S * (u128::from(self.psc_active) + 1)
    }

    /// Ticks elapsed since `base_ps` and the remainder in "ps * hz" units.
    fn elapsed_ticks(&self, now_ps: u64) -> (u128, u128) {
        if !self.running() || self.edge_clocked() {
            return (0, 0);
        }
        let dt = u128::from(now_ps.saturating_sub(self.base_ps)) * u128::from(self.kernel_hz());
        let tick = self.tick_ps_num();
        (dt / tick, dt % tick)
    }

    /// Center-aligned mode (CMS): 0 = edge-aligned.
    fn center(&self) -> u32 {
        (self.cr1 >> 5) & 3
    }

    /// Edge-aligned down-counting (DIR).
    fn counts_down(&self) -> bool {
        self.center() == 0 && self.cr1 & CR1_DIR != 0 && !self.edge_clocked()
    }

    /// Ticks of a counting cycle (from one wrap to the next).
    fn period(&self) -> u128 {
        let arr = u128::from(self.arr_active);
        if self.center() != 0 && !self.edge_clocked() { (2 * arr).max(1) } else { arr + 1 }
    }

    /// Counter value and direction (down) at phase `q` (0..=period) of the cycle.
    fn cnt_at(&self, q: u128) -> (u32, bool) {
        let arr = u128::from(self.arr_active);
        let q = q % self.period();
        if self.center() != 0 && !self.edge_clocked() {
            if q < arr { (q as u32, false) } else { ((2 * arr - q) as u32, true) }
        } else if self.counts_down() {
            ((arr - q.min(arr)) as u32, true)
        } else {
            (q as u32, false)
        }
    }

    /// Phase of counter value `cnt` counting in direction `down`.
    fn phase_of(&self, cnt: u32, down: bool) -> u128 {
        let (arr, cnt) = (u128::from(self.arr_active), u128::from(cnt));
        if self.center() != 0 && !self.edge_clocked() {
            let cnt = cnt.min(arr);
            if down && cnt > 0 { 2 * arr - cnt } else { cnt }
        } else if self.counts_down() {
            arr - cnt.min(arr)
        } else {
            cnt
        }
    }

    /// Phase at `base_ps` (a counter value above ARR counts as ARR).
    fn base_phase(&self) -> u128 {
        u128::from(self.cnt_base).min(self.period() - 1)
    }

    /// Ticks after `base_ps` until the counter next passes phase `q`
    /// (1..=period) after the ticks already processed.
    fn ticks_to_phase(&self, q: u128) -> u128 {
        let (base, period) = (self.base_phase(), self.period());
        let mut t = if q > base { q - base } else { period - base + q };
        if t <= self.checked {
            t += (self.checked - t) / period * period + period;
        }
        t
    }

    /// The turn at the top of a center-aligned cycle (an update event).
    fn top_phase(&self) -> Option<u128> {
        (self.center() != 0 && !self.edge_clocked()).then(|| u128::from(self.arr_active))
    }

    /// Update events per cycle.
    fn updates_per_period(&self) -> u128 {
        if self.top_phase().is_some() { 2 } else { 1 }
    }

    /// Phase at `now_ps`, as an offset from the base phase (may exceed a period).
    fn phase_now(&self, now_ps: u64) -> u128 {
        u128::from(self.cnt_base) + self.elapsed_ticks(now_ps).0
    }

    /// Current counter value without changing state.
    fn cnt(&self, now_ps: u64) -> u32 {
        let (ticks, _) = self.elapsed_ticks(now_ps);
        if self.arr_active == 0 || self.edge_clocked() {
            return self.cnt_base;
        }
        if self.center() != 0 || self.counts_down() {
            return self.cnt_at(u128::from(self.cnt_base) + ticks).0;
        }
        let pos = u128::from(self.cnt_base) + ticks;
        if pos <= u128::from(self.arr_active) {
            pos as u32
        } else {
            ((pos - u128::from(self.arr_active) - 1) % self.period()) as u32
        }
    }

    /// Current direction (true: down).
    fn dir_now(&self, now_ps: u64) -> bool {
        if self.encoder() {
            return self.cr1 & CR1_DIR != 0;
        }
        if self.center() != 0 && self.arr_active != 0 {
            return self.cnt_at(self.phase_now(now_ps)).1;
        }
        self.counts_down()
    }

    /// Re-anchors the lazy counter at `now_ps` at phase `cnt` (the counter
    /// value when counting up), keeping the fractional tick so that time does
    /// not drift.
    fn rebase(&mut self, now_ps: u64, cnt: u32) {
        let (_, rem) = self.elapsed_ticks(now_ps);
        let hz = u128::from(self.kernel_hz().max(1));
        self.base_ps = now_ps - (rem / hz) as u64;
        self.cnt_base = cnt;
        self.checked = 0;
    }

    /// Output compare channel `ch` with CCR preload (OCxPE).
    fn ccr_preload(&self, ch: usize) -> bool {
        self.compares(ch) && (self.ccmr[ch / 2] >> (8 * (ch % 2))) & 0x08 != 0
    }

    fn update_event(&mut self, set_flag: bool) {
        self.psc_active = self.psc;
        self.arr_active = self.arr;
        self.ccr_active = self.ccr;
        self.update_pwm(); // preloaded values take effect
        self.rep_left = self.rcr;
        if set_flag {
            self.sr |= SR_UIF;
        }
        if self.cr1 & CR1_OPM != 0 {
            self.cr1 &= !CR1_CEN;
        }
    }

    /// `n` counter overflows: whether one of them is an update event (the
    /// repetition counter of advanced timers skips `RCR` overflows each time).
    fn overflows(&mut self, n: u128) -> bool {
        let left = u128::from(self.rep_left);
        if n <= left {
            self.rep_left -= n as u32;
            return false;
        }
        let period = u128::from(self.rcr) + 1;
        self.rep_left = self.rcr - ((n - left - 1) % period) as u32;
        true
    }

    /// Channel `ch` is an output compare channel (CCxS = 00).
    fn compares(&self, ch: usize) -> bool {
        ch < self.channels && (self.ccmr[ch / 2] >> (8 * (ch % 2))) & 3 == 0
    }

    /// Counter value at which channel `ch` matches: CCR, or 0 (at the
    /// overflow) if CCR is above ARR, as measured on the board.
    fn match_value(&self, ch: usize) -> u32 {
        if self.ccr_active[ch] > self.arr_active { 0 } else { self.ccr_active[ch] }
    }

    /// Phases (1..=period) at which the counter reaches CCR of channel `ch`:
    /// counting up and counting down. `outputs` = for OCxREF (which follows
    /// both directions), else for the flag (center-aligned: as CMS selects).
    fn match_phases(&self, ch: usize, outputs: bool) -> [Option<u128>; 2] {
        if !self.compares(ch) {
            return [None, None];
        }
        let (arr, period) = (u128::from(self.arr_active), self.period());
        let at = |q: u128| if q == 0 { period } else { q };
        if self.center() != 0 && !self.edge_clocked() {
            let c = u128::from(self.ccr_active[ch]);
            if c > arr {
                return [None, None];
            }
            let (up, down) = (outputs || self.center() & 2 != 0, outputs || self.center() & 1 != 0);
            let up = (up && c < arr).then(|| at(c));
            let down = (down && c > 0).then_some(2 * arr - c);
            return [up, down];
        }
        if self.counts_down() {
            let c = u128::from(self.ccr_active[ch]);
            return [None, (c <= arr).then(|| at(arr - c))];
        }
        if outputs {
            return [self.oc_match_pos(ch), None];
        }
        let m = u128::from(self.match_value(ch));
        [Some(at(m)), None]
    }

    /// Flags the channels whose match the counter reaches between phases
    /// `from` (exclusive) and `to` (inclusive) of a cycle (`to` = period: the wrap).
    fn flag_matches(&mut self, from: u128, to: u128) {
        for ch in 0..4 {
            if self.match_phases(ch, false).into_iter().flatten().any(|m| from < m && m <= to) {
                self.sr |= 2 << ch;
            }
        }
    }

    /// Takes the input edges, then processes overflows and compare matches up to now.
    fn advance(&mut self, ctx: &mut DeviceCtx) {
        let now_ps = ctx.clock.time_ps();
        self.take_input_edges(ctx, now_ps);
        self.advance_to(ctx, now_ps);
    }

    /// Processes overflows and compare matches up to `now_ps`.
    fn advance_to(&mut self, ctx: &mut DeviceCtx, now_ps: u64) {
        self.take_captures(now_ps);
        if !self.running() || self.arr_active == 0 {
            self.next_event = None;
            self.push_outputs(ctx.clock, [now_ps; 4]);
            self.take_captures(now_ps);
            return;
        }
        let (ticks, _) = self.elapsed_ticks(now_ps);
        let base = self.base_phase();
        let to_overflow = self.period() - base;
        let mut edges = [now_ps; 4];
        let note = |edges: &mut [u64; 4], t: &Self, last: [Option<u128>; 4], base: u128| {
            for (e, pos) in edges.iter_mut().zip(last) {
                if let Some(pos) = pos {
                    *e = t.pos_ps(pos, base);
                }
            }
        };
        if ticks < to_overflow {
            self.flag_matches(base + self.checked, base + ticks);
            let last = self.oc_span(base + self.checked, base + ticks);
            note(&mut edges, self, last, base);
            self.top_update(base + self.checked, base + ticks, base, ctx);
            self.checked = ticks;
        } else {
            let period = self.period();
            self.flag_matches(base + self.checked, period);
            let last = self.oc_span(base + self.checked, period);
            note(&mut edges, self, last, base);
            self.top_update(base + self.checked, period, base, ctx);
            // Time of the (first) overflow, then restart from 0 there.
            let hz = u128::from(self.kernel_hz().max(1));
            let wrap_ps = self.base_ps + (to_overflow * self.tick_ps_num() / hz) as u64;
            let udis = self.cr1 & CR1_UDIS != 0;
            if !udis && self.overflows(1) {
                self.update_event(true);
                if self.trgo_on_update() {
                    self.emit_trgo(ctx.clock.cycle_at_ps(wrap_ps));
                }
            }
            self.base_ps = wrap_ps;
            self.cnt_base = 0;
            self.checked = 0;
            if self.running() {
                // Further complete periods with the (now active) settings.
                let (ticks, _) = self.elapsed_ticks(now_ps);
                let wraps = ticks / self.period();
                if wraps > 0 {
                    self.base_ps += (wraps * self.period() * self.tick_ps_num() / hz) as u64;
                    self.flag_matches(0, self.period());
                    for (e, changed) in edges.iter_mut().zip(self.oc_periods(wraps)) {
                        if changed {
                            *e = self.base_ps;
                        }
                    }
                    if !udis && self.overflows(wraps * self.updates_per_period()) {
                        self.update_event(true);
                    }
                }
                let (ticks, _) = self.elapsed_ticks(now_ps);
                self.flag_matches(0, ticks);
                let last = self.oc_span(0, ticks);
                note(&mut edges, self, last, 0);
                self.top_update(0, ticks, 0, ctx);
                self.checked = ticks;
            }
        }
        self.push_outputs(ctx.clock, edges);
        self.take_captures(now_ps);
        self.schedule(ctx);
    }

    /// The update event at the top of a center-aligned cycle, if the counter
    /// passes it between phases `from` and `to` (relative to `base`).
    fn top_update(&mut self, from: u128, to: u128, base: u128, ctx: &DeviceCtx) {
        let Some(top) = self.top_phase() else { return };
        if from < top && top <= to && self.cr1 & CR1_UDIS == 0 && self.overflows(1) {
            self.update_event(true);
            if self.trgo_on_update() {
                self.emit_trgo(ctx.clock.cycle_at_ps(self.pos_ps(top, base)));
            }
        }
    }

    /// Ticks after `base_ps` until the next compare match of an interrupt
    /// enabled or output driving channel.
    fn next_match_ticks(&self) -> Option<u128> {
        (0..4)
            .filter(|&ch| self.compares(ch))
            .flat_map(|ch| {
                let flags = (self.dier & (2 << ch) != 0).then(|| self.match_phases(ch, false));
                let out = self.oc_level(ch).is_some().then(|| self.match_phases(ch, true));
                flags.into_iter().chain(out).flatten().flatten()
            })
            .map(|m| self.ticks_to_phase(m))
            .min()
    }

    fn schedule(&mut self, ctx: &mut DeviceCtx) {
        self.next_event = None;
        if !self.running() || self.arr_active == 0 {
            return;
        }
        let driving = (0..self.channels).any(|ch| self.oc_level(ch).is_some() && matches!(self.oc_mode(ch), 6 | 7));
        let wake = self.dier & DIER_UIE != 0 || self.trgo_on_update() || driving;
        let overflow = wake.then(|| self.period() - self.base_phase());
        let top = self.top_phase().filter(|_| wake).map(|q| self.ticks_to_phase(q));
        let Some(ticks) = [overflow, top, self.next_match_ticks()].into_iter().flatten().min() else { return };
        self.next_event = Some(self.cycle_of_tick(ticks, ctx.clock).max(ctx.now() + 1));
    }

    /// First cycle at which `ticks` ticks after `base_ps` have elapsed.
    fn cycle_of_tick(&self, ticks: u128, clock: &Clock) -> u64 {
        let hz = u128::from(self.kernel_hz().max(1));
        let ps = u128::from(self.base_ps) + (ticks * self.tick_ps_num()).div_ceil(hz);
        clock.cycle_at_ps(u64::try_from(ps).unwrap_or(u64::MAX))
    }

    fn next_tick_cycle(&self, clock: &Clock) -> u64 {
        let (ticks, _) = self.elapsed_ticks(clock.time_ps());
        self.cycle_of_tick(ticks + 1, clock)
    }

    /// Next cycle at which SR may change (overflow or compare match).
    fn next_flag_cycle(&self, clock: &Clock) -> u64 {
        if self.arr_active == 0 {
            return u64::MAX;
        }
        let base = self.base_phase();
        let mut ticks = self.period() - base;
        let phases = (0..4).flat_map(|ch| self.match_phases(ch, false)).flatten().chain(self.top_phase());
        for m in phases {
            if m > base + self.checked {
                ticks = ticks.min(m - base);
            }
        }
        self.cycle_of_tick(ticks, clock)
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        let active = self.sr & self.dier;
        match self.cc_irq {
            Some(cc) => {
                ctx.set_irq_source(self.irq, self.irq_src, active & SR_UIF != 0);
                ctx.set_irq(cc, active & 0x1E != 0);
            }
            None => ctx.set_irq_source(self.irq, self.irq_src, active & 0x5F != 0),
        }
    }

    fn reg(&self, off: u32, now_ps: u64) -> u32 {
        match off {
            // DIR shows the direction while counting center-aligned
            0x00 if self.center() != 0 => (self.cr1 & !CR1_DIR) | (u32::from(self.dir_now(now_ps)) << 4),
            0x00 => self.cr1,
            0x04 => self.cr2,
            0x08 => self.smcr,
            0x0C => self.dier,
            0x10 => self.sr,
            0x18 => self.ccmr[0],
            0x1C => self.ccmr[1],
            0x20 => self.ccer,
            0x24 => self.cnt(now_ps),
            0x28 => self.psc,
            0x2C => self.arr,
            0x30 => self.rcr,
            0x34 => self.ccr[0],
            0x38 => self.ccr[1],
            0x3C => self.ccr[2],
            0x40 => self.ccr[3],
            0x44 => self.bdtr,
            0x48 => self.dcr,
            0x50 => self.or,
            _ => 0,
        }
    }

    fn warn_unsupported(&mut self, what: &str) {
        if !self.warned {
            self.warned = true;
            log::warn!("{}: {what} is not emulated yet", self.name);
        }
    }
}

impl Peripheral for GpTimer {
    fn name(&self) -> &str {
        self.name
    }

    fn read_stable_until(&self, off: u32, clock: &Clock) -> u64 {
        // CNT changes with every tick, SR (UIF) at the next overflow.
        if !self.running() {
            return u64::MAX;
        }
        match off & !3 {
            0x24 => self.next_tick_cycle(clock),
            0x10 => self.next_flag_cycle(clock),
            _ => u64::MAX,
        }
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        self.advance(ctx);
        let v = self.reg(off & !3, ctx.clock.time_ps());
        if let 0x34..=0x40 = off & !3 {
            // reading a captured value clears its flag
            let ch = ((off & !3) - 0x34) as usize / 4;
            if self.capture_source(ch).is_some() {
                self.sr &= !(2 << ch);
            }
        }
        self.sync_irq(ctx);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            if !self.warned_clock {
                self.warned_clock = true;
                log::warn!("{}: write while its clock is disabled (ignored)", self.name);
            }
            return;
        }
        self.advance(ctx);
        let now_ps = ctx.clock.time_ps();
        let aligned = off & !3;
        let v = merge_write(self.reg(aligned, now_ps), off, size, value);
        let mask = self.max();
        match aligned {
            0x00 => {
                let (cnt, down) = (self.cnt(now_ps), self.dir_now(now_ps));
                // DIR is read-only in encoder and center-aligned mode
                let dir = if self.encoder() || self.center() != 0 { self.cr1 & CR1_DIR } else { v & CR1_DIR };
                self.cr1 = (v & self.cr1_mask & !CR1_DIR) | dir;
                let down = if self.center() != 0 { down } else { self.counts_down() };
                self.rebase(now_ps, self.phase_of(cnt, down).min(u128::from(u32::MAX)) as u32);
                if self.cr1 & CR1_ARPE == 0 {
                    self.arr_active = self.arr;
                }
            }
            0x04 => self.cr2 = v,
            0x08 => {
                let (cnt, down) = (self.cnt(now_ps), self.dir_now(now_ps));
                self.smcr = v;
                self.rebase(now_ps, self.phase_of(cnt, down).min(u128::from(u32::MAX)) as u32);
                if v & 7 != 0 && !self.encoder() && self.ti_slave().is_none() {
                    self.warn_unsupported("slave mode with an internal trigger or ETR");
                }
                self.update_watch();
            }
            0x0C => self.dier = v & self.dier_mask,
            0x10 => self.sr &= v, // rc_w0
            0x14 => {
                if v & 1 != 0 {
                    // UG: re-initialise counter and prescaler, update shadow registers.
                    let flag = self.cr1 & CR1_URS == 0;
                    self.reinit(ctx, now_ps, flag);
                }
                // CCxG, COMG, TG, BG: set their flags
                let channels = (0x1Eu32 >> (4 - self.channels)) & 0x1E;
                self.sr |= v & (channels | 0xE0);
            }
            0x18 | 0x1C => {
                // CCxS can only be changed while the channel is off (CCxE = 0)
                let i = (aligned as usize - 0x18) / 4;
                let mut keep = 0;
                for half in 0..2 {
                    if self.ccer & (1 << (4 * (2 * i + half))) != 0 {
                        keep |= 3 << (8 * half);
                    }
                }
                self.ccmr[i] = (v & !keep) | (self.ccmr[i] & keep);
                for ch in [2 * i, 2 * i + 1] {
                    match self.oc_mode(ch) {
                        4 if self.compares(ch) => self.oc_ref[ch] = false,
                        5 if self.compares(ch) => self.oc_ref[ch] = true,
                        _ => {}
                    }
                }
                self.update_watch();
            }
            0x20 => {
                self.ccer = v;
                self.update_watch();
            }
            0x24 => {
                let (cnt, down) = (self.cnt(now_ps), self.dir_now(now_ps));
                for ch in 0..self.channels {
                    let (before, after) = (self.pwm_active(ch, cnt, down), self.pwm_active(ch, v & mask, down));
                    self.oc_compare_changed(ch, before, after);
                }
                let phase = self.phase_of(v & mask, down).min(u128::from(u32::MAX)) as u32;
                self.rebase(now_ps, phase);
                self.base_ps = now_ps;
            }
            0x28 => self.psc = v & 0xFFFF,
            0x2C => {
                self.arr = v & mask;
                if self.cr1 & CR1_ARPE == 0 {
                    self.arr_active = self.arr;
                }
            }
            // read-only while capturing
            0x34..=0x40 if self.capture_source((aligned - 0x34) as usize / 4).is_none() => {
                let ch = (aligned - 0x34) as usize / 4;
                self.ccr[ch] = v & mask;
                // with preload (OCxPE) the value waits for the next update event
                if !self.ccr_preload(ch) {
                    let (cnt, down) = (self.cnt(now_ps), self.dir_now(now_ps));
                    if ch < self.channels {
                        let after = if down { cnt <= v & mask } else { cnt < v & mask };
                        self.oc_compare_changed(ch, self.pwm_active(ch, cnt, down), after);
                    }
                    self.ccr_active[ch] = v & mask;
                }
            }
            0x30 if self.cc_irq.is_some() => self.rcr = v & 0xFF,
            0x44 if self.cc_irq.is_some() => self.bdtr = v,
            0x48 => self.dcr = v & 0x1F1F,
            0x50 => self.or = v,
            _ => {}
        }
        self.push_outputs(ctx.clock, [now_ps; 4]);
        self.take_captures(now_ps);
        self.schedule(ctx);
        self.sync_irq(ctx);
        self.update_pwm();
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        if self.next_event.is_some_and(|t| ctx.now() >= t) || !self.watched.is_empty() || !self.enc_watched.is_empty() {
            self.advance(ctx);
            self.sync_irq(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.next_event
    }

    fn reset(&mut self) {
        self.reset_regs();
        self.update_pwm();
        if let Some((pins, gpio)) = &self.inputs {
            let mut g = gpio.borrow_mut();
            for (ch, list) in pins.pins.iter().enumerate() {
                for &(p, pin) in *list {
                    g.drive_af(port(p), pin, pins.af, None, 0, 0);
                }
                self.oc_out[ch] = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::{SharedGpio, port};
    use crate::rcc::Clocks;
    use emu_core::nvic::Nvic;

    fn tim2() -> GpTimer {
        let clocks = SharedClocks::default();
        clocks.set(Clocks { apb1_timer: 90_000_000, apb1enr: 1, hclk: 180_000_000, ..Clocks::default() });
        GpTimer::new("TIM2", 28, true, ApbBus::Apb1, 0, clocks)
    }

    #[test]
    fn its_board_init_timer_counts_90_per_us() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        // initTimer(): PSC = 0, ARR = max, CEN
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x28, 4, 0);
            t.write(&mut ctx, 0x2C, 4, 0xFFFF_FFFF);
            t.write(&mut ctx, 0x00, 4, 1);
        }
        clock.advance(180_000_000 / 1000); // 1 ms
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        assert_eq!(t.read(&mut ctx, 0x24, 4), 90_000);
    }

    #[test]
    fn cnt_is_stable_until_the_next_prescaled_tick() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x00, 4, 1);
            t.write(&mut ctx, 0x28, 2, 90 * 10 - 1); // 10 us = 1800 cycles per tick
            t.write(&mut ctx, 0x14, 2, 1); // UG
        }
        for (now, cnt, until) in [(1000, 0, 1800), (1799, 0, 1800), (1800, 1, 3600), (5000, 2, 5400)] {
            clock.advance(now - clock.cycles());
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            assert_eq!(t.read(&mut ctx, 0x24, 4), cnt, "CNT at {now}");
            assert_eq!(t.read_stable_until(0x24, &clock), until, "CNT stable until, at {now}");
            assert_eq!(t.read_stable_until(0x00, &clock), u64::MAX);
        }
        // Stopped timer: constant.
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        t.write(&mut ctx, 0x00, 4, 0);
        assert_eq!(t.read_stable_until(0x24, &clock), u64::MAX);
    }

    #[test]
    fn stoppuhr_prescaler_via_ug_gives_10us_ticks() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x00, 4, 1);
            t.write(&mut ctx, 0x28, 2, 90 * 10 - 1); // STRH PSC
            t.write(&mut ctx, 0x14, 2, 1); // UG
        }
        clock.advance(180_000_000); // 1 s
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        assert_eq!(t.read(&mut ctx, 0x24, 4), 100_000);
        // UG set UIF (URS = 0)
        assert_eq!(t.read(&mut ctx, 0x10, 4) & 1, 1);
    }

    #[test]
    fn input_capture_of_a_button_edge() {
        static PINS: ChannelPins = ChannelPins { af: 3, pins: [&[('F', 6)], &[], &[], &[]] };
        let clocks = SharedClocks::default();
        clocks.set(Clocks { apb2_timer: 180_000_000, apb2enr: 1 << 17, hclk: 180_000_000, ..Clocks::default() });
        let gpio = SharedGpio::default();
        {
            let mut g = gpio.borrow_mut();
            let f = &mut g.ports[port('F')];
            f.moder = 2 << 12; // PF6: alternate function 3 (TIM10_CH1)
            f.afr[0] = 3 << 24;
            g.drive(port('F'), 6, Some(true)); // S6 released
        }
        let mut t = GpTimer::new("TIM10", 25, false, ApbBus::Apb2, 17, clocks)
            .with_channels(1)
            .with_inputs(&PINS, gpio.clone());
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        t.write(&mut ctx, 0x28, 4, 179); // 1 MHz
        t.write(&mut ctx, 0x18, 4, 1); // CC1S = 01: TI1
        t.write(&mut ctx, 0x20, 4, 0b1011); // CC1E, both edges
        t.write(&mut ctx, 0x18, 4, 0); // ignored while CC1E = 1
        assert_eq!(t.read(&mut ctx, 0x18, 4), 1);
        t.write(&mut ctx, 0x14, 4, 1);
        t.write(&mut ctx, 0x10, 4, 0);
        t.write(&mut ctx, 0x00, 4, 1);
        ctx.clock.advance(180 * 250); // 250 us
        gpio.borrow_mut().drive(port('F'), 6, Some(false)); // pressed: falling edge
        t.update(&mut ctx);
        assert_eq!(t.read(&mut ctx, 0x10, 4), 2, "CC1IF");
        ctx.clock.advance(180 * 100);
        gpio.borrow_mut().drive(port('F'), 6, Some(true)); // released before CCR1 was read
        t.update(&mut ctx);
        assert_eq!(t.read(&mut ctx, 0x10, 4), 0x202, "CC1IF + CC1OF");
        assert_eq!(t.read(&mut ctx, 0x34, 4), 350);
        assert_eq!(t.read(&mut ctx, 0x10, 4), 0x200, "reading CCR1 clears CC1IF");
        t.write(&mut ctx, 0x34, 4, 7); // read-only while capturing
        assert_eq!(t.read(&mut ctx, 0x34, 4), 350);
    }

    #[test]
    fn update_interrupt_every_millisecond() {
        let mut t = tim2();
        let mut clock = Clock::new(180_000_000);
        let mut nvic = Nvic::new();
        nvic.set_irq_enabled(28, true);
        {
            let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
            t.write(&mut ctx, 0x28, 4, 89); // 1 MHz
            t.write(&mut ctx, 0x2C, 4, 999); // 1 kHz
            t.write(&mut ctx, 0x14, 4, 1);
            t.write(&mut ctx, 0x10, 4, 0); // clear UIF from UG
            t.write(&mut ctx, 0x0C, 4, 1); // UIE
            t.write(&mut ctx, 0x00, 4, 1); // CEN
        }
        let ev = t.next_event().unwrap();
        assert_eq!(ev, 180_000);
        clock.advance(ev);
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        t.update(&mut ctx);
        assert!(nvic.is_pending(emu_core::nvic::IRQ0 + 28));
        assert_eq!(t.next_event(), Some(360_000));
    }
}
