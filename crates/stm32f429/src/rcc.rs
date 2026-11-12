//! Reset and clock control (RCC) and power controller (PWR) — with the
//! flash interface ([`crate::flash`]) everything `SystemClock_Config()`
//! waits on.
//!
//! Oscillators and PLLs report ready after their start-up / lock time (the
//! datasheet's typical values, see `READY_NS`). The RCC computes the
//! resulting bus clocks, publishes them in [`Clocks`] and sets the core clock
//! of the virtual time base.

use std::cell::Cell;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral, ResetKind};

pub const HSI_HZ: u32 = 16_000_000;
/// The NUCLEO-F429ZI feeds the 8 MHz MCO of the ST-LINK into OSC_IN (HSE bypass).
pub const HSE_HZ: u32 = 8_000_000;

/// Clock frequencies and peripheral clock enables, shared with other peripherals.
/// The APB bus a peripheral sits on (selects its clock and enable register).
#[derive(Debug, Clone, Copy)]
pub enum ApbBus {
    Apb1,
    Apb2,
}

impl ApbBus {
    /// Bit `bit` of the bus's RCC_APBxENR is set.
    pub fn enabled(self, c: &Clocks, bit: u32) -> bool {
        let enr = match self {
            Self::Apb1 => c.apb1enr,
            Self::Apb2 => c.apb2enr,
        };
        enr & (1 << bit) != 0
    }

    pub fn pclk(self, c: &Clocks) -> u32 {
        match self {
            Self::Apb1 => c.pclk1,
            Self::Apb2 => c.pclk2,
        }
    }

    /// Timer kernel clock (PCLK, doubled when the APB prescaler is not 1).
    pub fn timer_hz(self, c: &Clocks) -> u32 {
        match self {
            Self::Apb1 => c.apb1_timer,
            Self::Apb2 => c.apb2_timer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clocks {
    pub sysclk: u32,
    pub hclk: u32,
    pub pclk1: u32,
    pub pclk2: u32,
    /// Kernel clock of the timers on APB1 (TIM2–7, 12–14).
    pub apb1_timer: u32,
    /// Kernel clock of the timers on APB2 (TIM1, 8–11).
    pub apb2_timer: u32,
    pub ahb1enr: u32,
    pub ahb2enr: u32,
    pub apb1enr: u32,
    pub apb2enr: u32,
}

impl Default for Clocks {
    fn default() -> Self {
        Self {
            sysclk: HSI_HZ,
            hclk: HSI_HZ,
            pclk1: HSI_HZ,
            pclk2: HSI_HZ,
            apb1_timer: HSI_HZ,
            apb2_timer: HSI_HZ,
            ahb1enr: AHB1ENR_RESET,
            ahb2enr: 0,
            apb1enr: 0,
            apb2enr: 0,
        }
    }
}

pub type SharedClocks = Rc<Cell<Clocks>>;

const AHB1ENR_RESET: u32 = 0x0010_0000; // CCMDATARAMEN

const CR_HSION: u32 = 1 << 0;
const CR_HSEON: u32 = 1 << 16;
const CR_PLLON: u32 = 1 << 24;
const CR_PLLI2SON: u32 = 1 << 26;
const CR_PLLSAION: u32 = 1 << 28;
const CR_WRITABLE: u32 = 0x150D_00F9; // ON bits, HSITRIM, HSEBYP, CSSON
const CR_HSEBYP: u32 = 1 << 18;
/// ON bits of CR whose ready flag is the bit above, in `ready_at` order.
const CR_ON: [u32; 5] = [CR_HSION, CR_HSEON, CR_PLLON, CR_PLLI2SON, CR_PLLSAION];
/// Time from switching on to ready (ns): HSI start-up 2.2 µs typ (4 max),
/// HSE in bypass a few input clocks, a crystal 2 ms typ, PLL lock 100 µs
/// (datasheet: 75 min, 200 max; not measured on the board).
const READY_NS: [u64; 5] = [3_000, 2_000, 100_000, 100_000, 100_000];
const HSE_CRYSTAL_NS: u64 = 2_000_000;

pub struct Rcc {
    clocks: SharedClocks,
    cr: u32,
    pllcfgr: u32,
    cfgr: u32,
    cir: u32,
    // Index 0..5: AHB1, AHB2, AHB3, APB1, APB2
    rstr: [u32; 5],
    enr: [u32; 5],
    lpenr: [u32; 5],
    bdcr: u32,
    csr: u32,
    sscgr: u32,
    plli2scfgr: u32,
    pllsaicfgr: u32,
    dckcfgr: u32,
    /// Cycle at which each of `CR_ON` is ready (while it is on).
    ready_at: [u64; 5],
}

impl Rcc {
    pub fn new(clocks: SharedClocks) -> Self {
        let mut r = Self {
            clocks,
            cr: 0,
            pllcfgr: 0,
            cfgr: 0,
            cir: 0,
            rstr: [0; 5],
            enr: [0; 5],
            lpenr: [0; 5],
            bdcr: 0,
            csr: 0,
            sscgr: 0,
            plli2scfgr: 0,
            pllsaicfgr: 0,
            dckcfgr: 0,
            ready_at: [0; 5],
        };
        r.reset_regs();
        r
    }

    fn reset_regs(&mut self) {
        self.cr = 0x0000_0083;
        self.pllcfgr = 0x2400_3010;
        self.cfgr = 0;
        self.cir = 0;
        self.rstr = [0; 5];
        self.enr = [AHB1ENR_RESET, 0, 0, 0, 0];
        self.lpenr = [0x7E67_91FF, 0x0000_00F1, 0x0000_0001, 0x36FE_C9FF, 0x0477_7F33];
        self.bdcr = 0;
        self.csr = 0x0E00_0000;
        self.sscgr = 0;
        self.plli2scfgr = 0x2000_3000;
        self.pllsaicfgr = 0x2400_3000;
        self.dckcfgr = 0;
        self.ready_at = [0; 5];
    }

    /// CR with the ready flags: set once the source has been on for its
    /// start-up time.
    fn cr_value(&self, now: u64) -> u32 {
        let mut v = self.cr & !(0x2A02_0002);
        for (i, on) in CR_ON.into_iter().enumerate() {
            if self.cr & on != 0 && now >= self.ready_at[i] {
                v |= on << 1; // xxxRDY is the bit above xxxON
            }
        }
        v | (0x80 << 8) // HSICAL
    }

    /// Starts the start-up time of sources switched on by a CR write.
    fn write_cr(&mut self, v: u32, clock: &Clock) {
        let was = self.cr;
        self.cr = v & CR_WRITABLE;
        for (i, on) in CR_ON.into_iter().enumerate() {
            if self.cr & on != 0 && was & on == 0 {
                let ns = if on == CR_HSEON && self.cr & CR_HSEBYP == 0 { HSE_CRYSTAL_NS } else { READY_NS[i] };
                self.ready_at[i] = clock.cycles() + clock.ns_to_cycles(ns);
            }
        }
    }

    fn pll_out(&self) -> u32 {
        let src = if self.pllcfgr & (1 << 22) != 0 { HSE_HZ } else { HSI_HZ };
        let m = (self.pllcfgr & 0x3F).max(2);
        let n = (self.pllcfgr >> 6) & 0x1FF;
        let p = 2 * (((self.pllcfgr >> 16) & 3) + 1);
        (u64::from(src) * u64::from(n) / u64::from(m) / u64::from(p)) as u32
    }

    fn sysclk(&self) -> u32 {
        match self.cfgr & 3 {
            1 => HSE_HZ,
            2 => self.pll_out(),
            _ => HSI_HZ,
        }
    }

    fn update_clocks(&mut self, ctx: &mut DeviceCtx) {
        let sysclk = self.sysclk();
        let hpre = (self.cfgr >> 4) & 0xF;
        let ahb_div = if hpre < 8 { 1 } else { [2, 4, 8, 16, 64, 128, 256, 512][(hpre - 8) as usize] };
        let apb_div = |ppre: u32| if ppre < 4 { 1 } else { 1 << (ppre - 3) };
        let hclk = sysclk / ahb_div;
        let d1 = apb_div((self.cfgr >> 10) & 7);
        let d2 = apb_div((self.cfgr >> 13) & 7);
        let timer = |d: u32| if d == 1 { hclk } else { hclk / d * 2 };
        let clocks = Clocks {
            sysclk,
            hclk,
            pclk1: hclk / d1,
            pclk2: hclk / d2,
            apb1_timer: timer(d1),
            apb2_timer: timer(d2),
            ahb1enr: self.enr[0],
            ahb2enr: self.enr[1],
            apb1enr: self.enr[3],
            apb2enr: self.enr[4],
        };
        if clocks.hclk != self.clocks.get().hclk {
            log::info!("core clock {} MHz", f64::from(clocks.hclk) / 1e6);
        }
        self.clocks.set(clocks);
        ctx.clock.set_cpu_hz(hclk.max(1));
    }

    fn reg(&self, off: u32, now: u64) -> u32 {
        match off {
            0x00 => self.cr_value(now),
            0x04 => self.pllcfgr,
            // SWS mirrors SW: the switch happens immediately.
            0x08 => (self.cfgr & !0xC) | ((self.cfgr & 3) << 2),
            0x0C => self.cir,
            0x10 => self.rstr[0],
            0x14 => self.rstr[1],
            0x18 => self.rstr[2],
            0x20 => self.rstr[3],
            0x24 => self.rstr[4],
            0x30 => self.enr[0],
            0x34 => self.enr[1],
            0x38 => self.enr[2],
            0x40 => self.enr[3],
            0x44 => self.enr[4],
            0x50 => self.lpenr[0],
            0x54 => self.lpenr[1],
            0x58 => self.lpenr[2],
            0x60 => self.lpenr[3],
            0x64 => self.lpenr[4],
            // LSERDY follows LSEON
            0x70 => self.bdcr | ((self.bdcr & 1) << 1),
            // LSIRDY follows LSION
            0x74 => self.csr | ((self.csr & 1) << 1),
            0x80 => self.sscgr,
            0x84 => self.plli2scfgr,
            0x88 => self.pllsaicfgr,
            0x8C => self.dckcfgr,
            _ => 0,
        }
    }
}

impl Peripheral for Rcc {
    fn name(&self) -> &str {
        "RCC"
    }

    fn read_stable_until(&self, offset: u32, clock: &Clock) -> u64 {
        if offset & !3 != 0 {
            return u64::MAX;
        }
        // CR changes when a source becomes ready
        let now = clock.cycles();
        CR_ON
            .into_iter()
            .zip(self.ready_at)
            .filter(|&(on, at)| self.cr & on != 0 && at > now)
            .map(|(_, at)| at)
            .min()
            .unwrap_or(u64::MAX)
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        extract_read(self.reg(off & !3, ctx.now()), off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        let aligned = off & !3;
        let v = merge_write(self.reg(aligned, ctx.now()), off, size, value);
        match aligned {
            0x00 => self.write_cr(v, ctx.clock),
            0x04 => self.pllcfgr = v & 0x0F43_7FFF,
            0x08 => self.cfgr = v & !0xC,
            0x0C => self.cir = v & 0x3F00 & !(v >> 16), // interrupt enables; flags never set
            0x10 => self.rstr[0] = v,
            0x14 => self.rstr[1] = v,
            0x18 => self.rstr[2] = v,
            0x20 => self.rstr[3] = v,
            0x24 => self.rstr[4] = v,
            0x30 => self.enr[0] = v,
            0x34 => self.enr[1] = v,
            0x38 => self.enr[2] = v,
            0x40 => self.enr[3] = v,
            0x44 => self.enr[4] = v,
            0x50 => self.lpenr[0] = v,
            0x54 => self.lpenr[1] = v,
            0x58 => self.lpenr[2] = v,
            0x60 => self.lpenr[3] = v,
            0x64 => self.lpenr[4] = v,
            0x70 => self.bdcr = v & !0x2,
            0x74 => {
                self.csr = v & 1 | (self.csr & 0xFE00_0000);
                if v & (1 << 24) != 0 {
                    self.csr &= !0xFE00_0000; // RMVF clears the reset flags
                }
            }
            0x80 => self.sscgr = v,
            0x84 => self.plli2scfgr = v,
            0x88 => self.pllsaicfgr = v,
            0x8C => self.dckcfgr = v,
            _ => return,
        }
        self.update_clocks(ctx);
    }

    fn reset(&mut self) {
        self.reset_regs();
        self.clocks.set(Clocks::default());
    }

    /// The reset flags in CSR collect until RMVF; every reset but power-on
    /// also asserts NRST (measured: software reset 0x1400_0000, watchdog
    /// 0x2400_0000 after clearing the flags).
    fn system_reset(&mut self, kind: ResetKind) {
        let flags = self.csr & 0xFE00_0000;
        self.reset();
        const PIN: u32 = 1 << 26;
        self.csr = match kind {
            ResetKind::PowerOn => self.csr,
            ResetKind::Pin => flags | PIN,
            ResetKind::Software => flags | PIN | 1 << 28,
            ResetKind::Watchdog => flags | PIN | 1 << 29,
        };
    }
}

/// Power controller: voltage scaling and over-drive handshakes.
#[derive(Default)]
pub struct Pwr {
    cr: u32,
    csr: u32,
}

const PWR_CR_RESET: u32 = 0x0000_C000;

impl Pwr {
    pub fn new() -> Self {
        Self { cr: PWR_CR_RESET, csr: 0 }
    }

    fn csr_value(&self) -> u32 {
        let mut v = self.csr | (1 << 14); // VOSRDY
        if self.cr & (1 << 16) != 0 {
            v |= 1 << 16; // ODRDY follows ODEN
        }
        if self.cr & (1 << 17) != 0 {
            v |= 1 << 17; // ODSWRDY follows ODSWEN
        }
        v
    }
}

impl Peripheral for Pwr {
    fn name(&self) -> &str {
        "PWR"
    }

    fn read_stable_until(&self, _offset: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        let reg = match off & !3 {
            0x0 => self.cr,
            0x4 => self.csr_value(),
            _ => 0,
        };
        extract_read(reg, off, size)
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        match off & !3 {
            0x0 => self.cr = merge_write(self.cr, off, size, value) & 0x000F_FFFF & !0xC, // CWUF/CSBF are write-only
            0x4 => self.csr = merge_write(self.csr, off, size, value) & 0x0300,
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::Clock;
    use emu_core::nvic::Nvic;

    #[test]
    fn its_board_clock_config_gives_180_mhz() {
        let shared = SharedClocks::default();
        let mut rcc = Rcc::new(shared.clone());
        let mut clock = Clock::new(HSI_HZ);
        let mut nvic = Nvic::new();
        let mut ctx = DeviceCtx { clock: &mut clock, nvic: &mut nvic };
        // HSE bypass + on, wait for ready
        rcc.write(&mut ctx, 0x00, 4, 0x0005_0083);
        assert_eq!(rcc.read(&mut ctx, 0x00, 4) & (1 << 17), 0, "HSE not ready at once");
        let us = ctx.clock.ns_to_cycles(1_000);
        ctx.clock.advance(3 * us);
        assert_ne!(rcc.read(&mut ctx, 0x00, 4) & (1 << 17), 0, "HSERDY");
        // PLLM=8 PLLN=360 PLLP=2 PLLQ=7 from HSE
        rcc.write(&mut ctx, 0x04, 4, 8 | (360 << 6) | (1 << 22) | (7 << 24));
        rcc.write(&mut ctx, 0x00, 4, 0x0105_0083);
        assert_eq!(rcc.read(&mut ctx, 0x00, 4) & (1 << 25), 0, "PLL locks later");
        assert_eq!(rcc.read_stable_until(0, ctx.clock), ctx.now() + 100 * us, "until the lock");
        ctx.clock.advance(100 * us);
        assert_ne!(rcc.read(&mut ctx, 0x00, 4) & (1 << 25), 0, "PLLRDY");
        // HPRE=1, PPRE1=/4, PPRE2=/2, SW=PLL
        rcc.write(&mut ctx, 0x08, 4, (5 << 10) | (4 << 13) | 2);
        assert_eq!(rcc.read(&mut ctx, 0x08, 4) & 0xC, 0x8, "SWS = PLL");
        let c = shared.get();
        assert_eq!(c.hclk, 180_000_000);
        assert_eq!(c.pclk1, 45_000_000);
        assert_eq!(c.apb1_timer, 90_000_000);
        assert_eq!(c.pclk2, 90_000_000);
        assert_eq!(clock.cpu_hz(), 180_000_000);
    }
}
