//! Plugins: external hardware simulated on a thread of its own (such as a
//! Raspberry Pi Pico), with its own clock, wired to header pins of the board.
//!
//! The plugin computes ahead of the board and hands over its pin changes
//! with their times; the board applies each at that time of its own virtual
//! time, so it sees the signals as they were generated, however fast either
//! side runs on the host. Plugins only drive pins (they do not see the
//! board's outputs), so they never wait for the board: the board waits when
//! it gets ahead of what the plugin has computed. While the board does not
//! run (paused, halted by a debugger, not created yet), the plugin runs in
//! real time, and its pins change as it goes.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use emu_core::DeviceCtx;
use stm32f429::gpio::{SharedGpio, port};

use crate::devices::PinDevice;

/// How far (virtual time, ps) a plugin may run ahead of the board.
const LOOKAHEAD_PS: u64 = 20_000_000_000;
/// The board counts as not running when it has not advanced for this long.
const BOARD_IDLE: Duration = Duration::from_millis(100);
/// How far a plugin running in real time may get behind before it skips.
const MAX_REALTIME_LAG_PS: u64 = 50_000_000_000;

/// A pin change of a plugin output: time (plugin's clock, ps), output, level
/// (`None`: not driven).
pub type Edge = (u64, usize, Option<bool>);

struct State {
    /// The plugin's time and the board's time (ps) when they last reported.
    plugin_ps: u64,
    board_ps: u64,
    /// When the board last advanced.
    board_seen: Option<Instant>,
    board_waiting: bool,
    /// A board device is connected.
    attached: bool,
    /// Board time = plugin time + offset.
    offset: i128,
    /// Changes not yet applied (board time, output, level).
    edges: VecDeque<(u64, usize, Option<bool>)>,
    /// Each output's level after the last change.
    levels: Vec<Option<bool>>,
    /// Real-time reference (wall clock, plugin time) while the board does not run.
    realtime: Option<(Instant, u64)>,
    /// The plugin has ended (stopped or crashed).
    ended: bool,
}

impl State {
    fn board_time(&self, plugin_ps: u64) -> u64 {
        (i128::from(plugin_ps) + self.offset).clamp(0, i128::from(u64::MAX)) as u64
    }

    fn board_running(&self) -> bool {
        self.attached && (self.board_waiting || self.board_seen.is_some_and(|t| t.elapsed() < BOARD_IDLE))
    }

    /// The plugin's present is the board's present from now on; pending
    /// changes take effect at once.
    fn resync(&mut self) {
        self.offset = i128::from(self.board_ps) - i128::from(self.plugin_ps);
        let now = self.board_ps;
        for e in &mut self.edges {
            e.0 = now;
        }
    }
}

/// The connection between a plugin's thread and the board.
pub struct Link {
    state: Mutex<State>,
    changed: Condvar,
}

impl Link {
    /// A link for a plugin with `outputs` pins.
    pub fn new(outputs: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                plugin_ps: 0,
                board_ps: 0,
                board_seen: None,
                board_waiting: false,
                attached: false,
                offset: 0,
                edges: VecDeque::new(),
                levels: vec![None; outputs],
                realtime: None,
                ended: false,
            }),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ----- plugin side ----------------------------------------------------

    /// Until when (plugin time) the plugin, now at `plugin_ps`, may run. If
    /// it may not run on now, this waits up to `max_wait` for the board
    /// first (and may still return `plugin_ps`).
    pub fn plugin_target(&self, plugin_ps: u64, max_wait: Duration) -> u64 {
        let mut st = self.lock();
        for attempt in 0..2 {
            let target = if st.board_running() {
                st.realtime = None;
                let board_target = st.board_ps.saturating_add(LOOKAHEAD_PS);
                (i128::from(board_target) - st.offset).clamp(0, i128::from(u64::MAX)) as u64
            } else {
                let (since, at) = *st.realtime.get_or_insert((Instant::now(), plugin_ps));
                let target = at.saturating_add(since.elapsed().as_nanos() as u64 * 1000);
                if target > plugin_ps + MAX_REALTIME_LAG_PS {
                    // the plugin is slower than real time: keep the pace from here
                    st.realtime = Some((Instant::now(), plugin_ps));
                }
                target.min(plugin_ps + MAX_REALTIME_LAG_PS)
            };
            if target > plugin_ps || attempt == 1 {
                return target.max(plugin_ps);
            }
            st = self.changed.wait_timeout(st, max_wait).unwrap_or_else(|e| e.into_inner()).0;
        }
        plugin_ps
    }

    /// The plugin has run until `plugin_ps`; `edges` are its pin changes
    /// since the last call, in time order.
    pub fn publish(&self, plugin_ps: u64, edges: impl IntoIterator<Item = Edge>) {
        let mut st = self.lock();
        st.plugin_ps = plugin_ps;
        if !st.board_running() {
            // the board stands still: the changes happen at its present
            st.resync();
        }
        for (ps, output, level) in edges {
            let Some(l) = st.levels.get_mut(output) else { continue };
            *l = level;
            let at = st.board_time(ps);
            st.edges.push_back((at, output, level));
        }
        drop(st);
        self.changed.notify_all();
    }

    /// The state of the link, for debugging.
    pub fn describe(&self) -> String {
        let st = self.lock();
        format!(
            "board at {:.3} s (seen {:?} ago, waiting {}, attached {}, running {}), offset {:.3} s, {} changes queued",
            st.board_ps as f64 / 1e12,
            st.board_seen.map(|t| t.elapsed()),
            st.board_waiting,
            st.attached,
            st.board_running(),
            st.offset as f64 / 1e12,
            st.edges.len()
        )
    }

    /// The plugin has ended: the board no longer waits for it.
    pub fn end(&self) {
        self.lock().ended = true;
        self.changed.notify_all();
    }

    // ----- board side -----------------------------------------------------

    /// Brings the board's side to board time `board_ps`: waits until the
    /// plugin has computed that far, then hands over the changes up to then.
    /// Returns the board time of the next change or of the plugin's horizon.
    fn board_update(&self, board_ps: u64, mut apply: impl FnMut(u64, usize, Option<bool>)) -> Option<u64> {
        let mut st = self.lock();
        if board_ps < st.board_ps || st.board_seen.is_none() {
            // a new board or a reset of the simulation: start from here
            st.board_ps = board_ps;
            st.resync();
        }
        if board_ps > st.board_ps || st.board_seen.is_none() {
            st.board_seen = Some(Instant::now());
        }
        st.board_ps = board_ps;
        self.changed.notify_all();
        while !st.ended && st.board_time(st.plugin_ps) < board_ps {
            st.board_waiting = true;
            st = self.changed.wait_timeout(st, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        }
        st.board_waiting = false;
        while let Some(&(at, output, level)) = st.edges.front()
            && at <= board_ps
        {
            st.edges.pop_front();
            apply(at, output, level);
        }
        if st.ended {
            return st.edges.front().map(|e| e.0);
        }
        let horizon = st.board_time(st.plugin_ps);
        Some(st.edges.front().map_or(horizon, |e| e.0.min(horizon)))
    }

    fn attach(&self) -> Vec<Option<bool>> {
        let mut st = self.lock();
        st.attached = true;
        st.board_seen = None;
        st.edges.clear();
        st.levels.clone()
    }

    fn detach(&self) {
        let mut st = self.lock();
        st.attached = false;
        st.board_waiting = false;
        drop(st);
        self.changed.notify_all();
    }
}

/// A board pin (port, pin) and its level while the plugin output wired to
/// it does not drive it.
pub type Wire<P> = (P, Option<bool>);

/// The board's end of a [`Link`]: drives the pins the plugin's outputs are
/// wired to. An output that is not driven leaves the pin to the board (the
/// pull-up of the buttons S0–S7, for instance).
pub struct PluginPins {
    link: Arc<Link>,
    gpio: SharedGpio,
    /// Board pin (port index, pin) of each plugin output, and its level when
    /// the output does not drive it.
    wiring: Vec<Option<Wire<(usize, u32)>>>,
    next_ps: Option<u64>,
    /// `next_ps` in cycles, at the last update.
    next_cycle: Option<u64>,
}

impl PluginPins {
    /// `wiring[output]`: the board pin wired to that plugin output and the
    /// level of the pin when the output does not drive it.
    pub fn new(link: Arc<Link>, gpio: SharedGpio, wiring: Vec<Option<Wire<(char, u32)>>>) -> Self {
        let wiring: Vec<_> = wiring.into_iter().map(|w| w.map(|((p, pin), idle)| ((port(p), pin), idle))).collect();
        let levels = link.attach();
        let mut pins = Self { link, gpio, wiring, next_ps: Some(0), next_cycle: Some(0) };
        for (output, level) in levels.into_iter().enumerate() {
            pins.drive(output, level, 0);
        }
        pins
    }

    fn drive(&mut self, output: usize, level: Option<bool>, cycle: u64) {
        if let Some(Some(((port, pin), idle))) = self.wiring.get(output) {
            self.gpio.borrow_mut().drive_at(*port, *pin, level.or(*idle), cycle);
        }
    }
}

impl PinDevice for PluginPins {
    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        let clock = &*ctx.clock;
        let mut changes = Vec::new();
        self.next_ps = self.link.board_update(clock.time_ps(), |at, output, level| changes.push((at, output, level)));
        for (at, output, level) in changes {
            log::trace!("plugin output {output} = {level:?} at {at} ps (board now {} ps)", clock.time_ps());
            let cycle = clock.cycle_at_ps(at).min(now);
            self.drive(output, level, cycle);
        }
        self.next_cycle = self.next_ps.map(|ps| ctx.clock.cycle_at_ps(ps).max(now + 1));
    }

    fn next_event(&self) -> Option<u64> {
        self.next_cycle
    }
}

impl Drop for PluginPins {
    fn drop(&mut self) {
        self.link.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_reach_the_board_at_their_time() {
        let link = Link::new(1);
        let gpio = SharedGpio::default();
        let mut pins = PluginPins::new(link.clone(), gpio.clone(), vec![Some((('F', 0), Some(true)))]);
        let mut clock = emu_core::Clock::new(1_000_000); // 1 µs per cycle
        let mut nvic = emu_core::nvic::Nvic::new();
        // the board starts; the plugin computes 5 µs with a change at 3 µs
        pins.update(&mut DeviceCtx { clock: &mut clock, nvic: &mut nvic });
        link.publish(5_000_000, [(3_000_000, 0, Some(false))]);
        clock.advance(2);
        pins.update(&mut DeviceCtx { clock: &mut clock, nvic: &mut nvic });
        assert!(gpio.borrow().level(port('F'), 0), "not yet");
        assert_eq!(pins.next_event(), Some(3), "the board looks again at the change");
        clock.advance(1);
        pins.update(&mut DeviceCtx { clock: &mut clock, nvic: &mut nvic });
        assert!(!gpio.borrow().level(port('F'), 0));
        assert_eq!(pins.next_event(), Some(5), "the plugin's horizon");
        // released: the pull-up
        link.publish(6_000_000, [(5_500_000, 0, None)]);
        clock.advance(3);
        pins.update(&mut DeviceCtx { clock: &mut clock, nvic: &mut nvic });
        assert!(gpio.borrow().level(port('F'), 0));
    }

    #[test]
    fn the_board_waits_for_the_plugin() {
        let link = Link::new(1);
        let gpio = SharedGpio::default();
        let mut pins = PluginPins::new(link.clone(), gpio.clone(), vec![Some((('F', 1), Some(true)))]);
        let plugin = {
            let link = link.clone();
            std::thread::spawn(move || {
                let mut t = 0;
                while t < 10_000_000 {
                    let target = link.plugin_target(t, Duration::from_millis(10)).min(t + 1_000_000);
                    t = target;
                    link.publish(t, [(t, 0, Some(t % 2_000_000 == 0))]);
                }
                link.end();
            })
        };
        let mut clock = emu_core::Clock::new(1_000_000);
        let mut nvic = emu_core::nvic::Nvic::new();
        pins.update(&mut DeviceCtx { clock: &mut clock, nvic: &mut nvic });
        clock.advance(10);
        pins.update(&mut DeviceCtx { clock: &mut clock, nvic: &mut nvic });
        plugin.join().unwrap();
        assert!(gpio.borrow().level(port('F'), 1), "the change at 10 µs (even) is applied");
    }
}
