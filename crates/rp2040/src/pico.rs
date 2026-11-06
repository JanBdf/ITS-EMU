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

pub struct Pico {
    /// Core 0, which owns the flash and RAM, and core 1.
    cores: [Machine; 2],
    mcu: Rp2040,
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
        let core1 = Machine::new_sharing_memory(crate::machine_config(1), bus1, &core0)?;
        let mut pico = Self { cores: [core0, core1], mcu };
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
            // the core behind, up to a turn past the other
            let n = usize::from(t[1] < t[0]);
            if t[n] >= ps {
                break;
            }
            let target = ps.min(t[1 - n] + TURN_PS);
            let clock = self.cores[n].clock();
            let budget = clock.cycle_at_ps(target).saturating_sub(clock.cycles()).max(1);
            match self.cores[n].run(budget) {
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
        self.mcu.pins.borrow_mut().reset();
    }

    /// Core `n`'s PC.
    pub fn pc(&self, n: usize) -> u32 {
        self.cores[n].pc()
    }
}
