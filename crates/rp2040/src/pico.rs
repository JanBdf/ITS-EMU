//! The Raspberry Pi Pico: an RP2040 with its flash programmed from a UF2
//! file, its USB serial port and its GPIOs.

use emu_core::{Machine, MachineError, StopReason};

use crate::pins::{NUM_GPIO, PinEdge};
use crate::usb::SharedSerial;
use crate::{FLASH_BASE, FLASH_SIZE, ROM_BASE, Rp2040, uf2};

#[derive(Debug, thiserror::Error)]
pub enum PicoError {
    #[error(transparent)]
    Uf2(#[from] uf2::Uf2Error),
    #[error(transparent)]
    Machine(#[from] MachineError),
}

/// How far one core runs ahead of the other (ps): a turn ends sooner when
/// the core sleeps, and accesses to shared state take effect up to a turn late.
const TURN_PS: u64 = 10_000_000;
/// How far a core may run ahead of the other while that one sleeps (ps).
/// Its turn ends sooner when it may wake the sleeping core (a FIFO write,
/// an interrupt the sleeper has enabled, SEV, a spinlock release), so the
/// sleeping core sees that as soon as it would on the chip.
const SLEEP_TURN_PS: u64 = 1_000_000_000;
/// End of the boot ROM. A core asleep in it waits to be launched, which
/// only a write to its FIFO does: SEV and spinlocks (the SDK's mutexes use
/// them all the time) do not wake it.
const ROM_END: u32 = 0x4000;

pub struct Pico {
    /// Core 0, which owns the flash and RAM, and core 1.
    cores: [Machine; 2],
    mcu: Rp2040,
    /// The other core did something that may wake this one since it last ran.
    woken: [bool; 2],
}

impl Pico {
    /// A Pico with `bootrom` (16 KiB) and the flash programmed from `uf2`,
    /// reset and ready to run.
    pub fn new(bootrom: &[u8], uf2_file: &[u8]) -> Result<Self, PicoError> {
        let blocks = uf2::parse(uf2_file, FLASH_BASE, FLASH_SIZE)?;
        let ([bus0, bus1], mcu) = crate::build();
        let mut core0 = Machine::new(crate::machine_config(0), bus0)?;
        core0.load_bin(ROM_BASE, bootrom)?;
        for (addr, data) in &blocks {
            core0.load_bin(*addr, data)?;
        }
        core0.set_multicore(true);
        mcu.dma.borrow_mut().set_memory(core0.memory_view());
        let mut core1 = Machine::new_sharing_memory(crate::machine_config(1), bus1, &core0)?;
        core0.set_sev_signal(mcu.event[0].clone());
        core1.set_sev_signal(mcu.event[1].clone());
        let mut pico = Self { cores: [core0, core1], mcu, woken: [false; 2] };
        pico.reset();
        Ok(pico)
    }

    /// Fast-forwarding of idle loops on both cores (on by default; the
    /// result is the same without it, only slower).
    pub fn set_idle_skipping(&mut self, on: bool) {
        for core in &mut self.cores {
            core.system_mut().idle_skipping = on;
        }
    }

    /// Core 0.
    pub fn machine(&self) -> &Machine {
        &self.cores[0]
    }

    pub fn core(&self, n: usize) -> &Machine {
        &self.cores[n]
    }

    /// Time since power-on in picoseconds (of the core behind).
    pub fn time_ps(&self) -> u64 {
        self.cores.iter().map(|c| c.clock().time_ps()).min().unwrap_or(0)
    }

    /// Runs both cores until `ps` (time since power-on) or until one stops
    /// (the number of that core and why).
    pub fn run_until_ps(&mut self, ps: u64) -> Result<(), (usize, StopReason)> {
        loop {
            let t = [0, 1].map(|n| self.cores[n].clock().time_ps());
            // asleep in WFI/WFE, and nothing has happened that may wake it
            let sleeps = [0, 1].map(|n| self.cores[n].is_asleep() && !self.woken[n]);
            // the core behind (at a tie the one awake)
            let n = if t[0] == t[1] { usize::from(sleeps[0] && !sleeps[1]) } else { usize::from(t[1] < t[0]) };
            if t[n] >= ps {
                break;
            }
            let o = 1 - n;
            if sleeps[n] && !sleeps[o] {
                // The other core got this far without waking this one: it
                // slept all the while (running it would only repeat its wait).
                let cycle = self.cores[n].clock().cycle_at_ps(t[o].min(ps));
                self.cores[n].sleep_until(cycle);
                if self.cores[n].is_asleep() {
                    continue;
                }
                // its SysTick woke it: it runs from there
            }
            // up to a turn past the other core, or further while that one sleeps
            let mut target = ps.min(t[o] + TURN_PS);
            if sleeps[o] {
                let mut until = ps.min(t[n] + SLEEP_TURN_PS);
                if let Some(c) = self.cores[o].next_core_event() {
                    until = until.min(self.cores[o].clock().ps_at_cycle(c));
                }
                target = target.max(until);
            }
            // what may wake the other core
            let events_wake = self.cores[o].pc() >= ROM_END;
            let (wake, event) = (&self.mcu.wake[n], &self.mcu.event[n]);
            wake.set(false);
            event.set(false);
            let core = &mut self.cores[n];
            core.set_end_run_signals(if sleeps[o] {
                [Some(wake.clone()), events_wake.then(|| event.clone())]
            } else {
                [None, None]
            });
            let clock = core.clock();
            let budget = clock.cycle_at_ps(target).saturating_sub(clock.cycles()).max(1);
            let result = core.run(budget);
            // it has run its code again after what the other core did
            self.woken[n] = false;
            if wake.get() || (events_wake && event.get()) {
                self.woken[o] = true;
            }
            match result {
                StopReason::Budget => {}
                other => return Err((n, other)),
            }
        }
        // the PIOs and the USB host up to now
        self.cores[0].update_devices();
        Ok(())
    }

    /// Pin changes since the last call, in time order.
    pub fn take_edges(&mut self) -> Vec<PinEdge> {
        self.mcu.pins.borrow_mut().take_edges()
    }

    /// What each GPIO drives now (`None`: input / not driven).
    pub fn outputs(&self) -> [Option<bool>; NUM_GPIO] {
        self.mcu.pins.borrow().outputs()
    }

    /// Voltages at the ADC inputs (GPIO 26..29) and the temperature at the
    /// sensor.
    pub fn analog(&self) -> &crate::adc::SharedAnalog {
        &self.mcu.analog
    }

    /// The USB CDC serial port.
    pub fn serial(&self) -> &SharedSerial {
        &self.mcu.serial
    }

    /// What the firmware used that is not emulated, since the last call
    /// (each thing once per Pico).
    pub fn take_notices(&mut self) -> Vec<String> {
        self.mcu.notices.take()
    }

    /// The RUN pin: restarts the chip (flash keeps the firmware).
    pub fn reset(&mut self) {
        for core in &mut self.cores {
            core.reset();
        }
        self.woken = [false; 2];
        self.mcu.pins.borrow_mut().reset();
    }

    /// Core `n`'s PC.
    pub fn pc(&self, n: usize) -> u32 {
        self.cores[n].pc()
    }
}
