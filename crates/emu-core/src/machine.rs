//! Cortex-M4F machine on top of the Unicorn CPU emulator.
//!
//! Unicorn executes instructions. Everything around it — memory map, private
//! peripheral bus, NVIC, exception entry/return, time keeping — lives here.

use std::collections::BTreeSet;

use unicorn_engine::{Arch, ArmCpuModel, HookType, MemType, Mode, Prot, RegisterARM, UcHookId, Unicorn, uc_error};

use crate::bus::{Bus, DeviceCtx};
use crate::clock::Clock;
use crate::elf::ElfImage;
use crate::exception::{self, AccessError, CoreAccess, CoreReg, ExceptionError};
use crate::nvic::{self, Nvic};
use crate::scs::{self, PPB_BASE, PPB_SIZE, Scs, ScsAction};

/// Address space layout of the emulated MCU.
#[derive(Debug, Clone)]
pub struct MemoryMap {
    pub flash_base: u32,
    pub flash_size: u32,
    /// Flash is also visible at address 0 (boot from main flash).
    pub flash_alias_at_zero: bool,
    /// RAM regions (base, size), mapped read/write/execute.
    pub ram: Vec<(u32, u32)>,
    /// Read-only regions filled with zeros (system memory, OTP, option bytes).
    pub rom_zero: Vec<(u32, u32)>,
    /// SRAM bit-band region (1 MB) and its 32 MB alias, if present.
    pub sram_bitband: Option<(u32, u32)>,
    /// Peripheral window dispatched to the [`Bus`].
    pub periph_base: u32,
    pub periph_size: u32,
}

#[derive(Debug, Clone)]
pub struct MachineConfig {
    pub memory: MemoryMap,
    /// Core clock after reset (HSI on STM32F4: 16 MHz).
    pub reset_cpu_hz: u32,
    /// Maximum number of cycles per Unicorn run when nothing is pending.
    pub quantum: u64,
    /// Shorter slice used while an exception is pending but masked, so that
    /// unmasking (CPSIE, BASEPRI writes) is noticed soon.
    pub masked_quantum: u64,
}

/// Why [`Machine::run`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The requested cycle budget was used up.
    Budget,
    /// A `BKPT` instruction was executed at `pc`.
    Breakpoint { pc: u32 },
    /// The core locked up (fault while handling a fault, ...).
    Lockup { pc: u32, reason: String },
    /// A debugger breakpoint ([`Machine::add_breakpoint`]) was reached. The
    /// instruction at `pc` has not been executed yet.
    DebugBreakpoint { pc: u32 },
    /// [`Machine::step`] executed one instruction.
    Step,
    /// A watched address ([`Machine::add_watchpoint`]) was accessed; `pc` is
    /// the instruction after the access.
    Watchpoint { pc: u32, addr: u32, kind: WatchKind },
}

/// Accesses a watchpoint reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    Write,
    Read,
    Access,
}

struct Watchpoint {
    addr: u32,
    len: u32,
    kind: WatchKind,
    hook: UcHookId,
}

#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    #[error("unicorn error: {0:?}")]
    Unicorn(uc_error),
    #[error(transparent)]
    Elf(#[from] crate::elf::ElfError),
    #[error("segment at {addr:#010x} (+{len:#x}) is outside of mapped memory")]
    SegmentOutOfRange { addr: u32, len: usize },
}

impl From<uc_error> for MachineError {
    fn from(e: uc_error) -> Self {
        MachineError::Unicorn(e)
    }
}

/// Machine state reachable from Unicorn callbacks.
pub struct System {
    pub clock: Clock,
    pub nvic: Nvic,
    pub scs: Scs,
    pub bus: Bus,
    config: MachineConfig,
    /// Unicorn is stopped once the cycle counter reaches this value.
    slice_end: u64,
    /// Instruction count per translated block (direct-mapped cache).
    block_insns: BlockCache,
    /// Last invalid memory access reported by Unicorn (address, is_write, is_fetch).
    last_invalid: Option<(u32, bool, bool)>,
    stop: Option<StopReason>,
    reset_requested: bool,
    idle: IdleDetector,
    /// Fast-forward idle loops to the next event (disable for debugging).
    pub idle_skipping: bool,
    /// [`Machine::step`] is executing a single instruction.
    stepping: bool,
    /// A watchpoint (start address, kind) was hit during the current run.
    watch_hit: Option<(u32, WatchKind)>,
}

impl System {
    /// Ends the current Unicorn run at the next block boundary. Safe to call
    /// from MMIO callbacks (unlike `emu_stop`, which would re-execute the
    /// current instruction).
    pub fn request_stop(&mut self) {
        self.slice_end = 0;
    }

    /// Cycles skipped by idle-loop fast-forwarding since reset.
    pub fn idle_skipped_cycles(&self) -> u64 {
        self.idle.skipped_cycles
    }

    fn ctx(&mut self) -> (DeviceCtx<'_>, &mut Bus) {
        (DeviceCtx { clock: &mut self.clock, nvic: &mut self.nvic }, &mut self.bus)
    }
}

pub struct Machine {
    uc: Unicorn<'static, System>,
    /// Backing store for flash; mapped into Unicorn with `mem_map_ptr`.
    /// Declared after `uc` so it is dropped after the emulator.
    flash: Box<[u8]>,
    /// Debugger breakpoints, implemented as Unicorn exits: execution stops
    /// before an instruction at one of these addresses.
    breakpoints: BTreeSet<u32>,
    watchpoints: Vec<Watchpoint>,
    /// Code hook on every instruction while watchpoints exist: stops right
    /// after the instruction that hit one.
    watch_stopper: Option<UcHookId>,
    /// Last resume request of the GDB client, taken by the server.
    #[cfg(feature = "gdb")]
    pub(crate) gdb_resume: Option<crate::gdb::Resume>,
}

/// Wrapper implementing [`CoreAccess`] for Unicorn.
struct UcCore<'a, 'b>(&'a mut Unicorn<'b, System>);

fn uc_reg(r: CoreReg) -> i32 {
    match r {
        CoreReg::R(n) => RegisterARM::R0 as i32 + i32::from(n),
        CoreReg::Sp => RegisterARM::SP as i32,
        CoreReg::Lr => RegisterARM::LR as i32,
        CoreReg::Pc => RegisterARM::PC as i32,
        CoreReg::Xpsr => RegisterARM::XPSR as i32,
        CoreReg::Msp => RegisterARM::MSP as i32,
        CoreReg::Psp => RegisterARM::PSP as i32,
        CoreReg::Control => RegisterARM::CONTROL as i32,
        CoreReg::Primask => RegisterARM::PRIMASK as i32,
        CoreReg::Faultmask => RegisterARM::FAULTMASK as i32,
        CoreReg::Basepri => RegisterARM::BASEPRI as i32,
        CoreReg::Fpscr => RegisterARM::FPSCR as i32,
        CoreReg::S(n) => RegisterARM::S0 as i32 + i32::from(n),
    }
}

impl CoreAccess for UcCore<'_, '_> {
    fn reg(&self, r: CoreReg) -> u32 {
        self.0.reg_read(uc_reg(r)).unwrap_or(0) as u32
    }

    fn set_reg(&mut self, r: CoreReg, v: u32) {
        let id = match r {
            // Writing the full xPSR (incl. IPSR/EPSR) needs the NZCVQG variant.
            CoreReg::Xpsr => RegisterARM::XPSR_NZCVQG as i32,
            other => uc_reg(other),
        };
        let _ = self.0.reg_write(id, u64::from(v));
    }

    fn read_u32(&mut self, addr: u32) -> Result<u32, AccessError> {
        let mut b = [0u8; 4];
        self.0.mem_read(u64::from(addr), &mut b).map_err(|_| AccessError::Bus(addr))?;
        Ok(u32::from_le_bytes(b))
    }

    fn write_u32(&mut self, addr: u32, v: u32) -> Result<(), AccessError> {
        self.0.mem_write(u64::from(addr), &v.to_le_bytes()).map_err(|_| AccessError::Bus(addr))
    }
}

/// Per-block facts derived from the code bytes.
#[derive(Debug, Clone, Copy)]
struct BlockInfo {
    insns: u32,
    /// The block contains stores (STR, PUSH, STM, VSTR, ...).
    stores: bool,
    /// The block may change state that idle detection does not compare
    /// (MSR, CPS, SVC, floating-point registers). Never part of an idle loop.
    side_effects: bool,
}

/// Direct-mapped cache: block (address, size) -> [`BlockInfo`].
struct BlockCache {
    entries: Vec<(u32, u16, u16)>,
}

impl BlockCache {
    const BITS: u32 = 16;
    const STORES: u16 = 0x4000;
    const SIDE_EFFECTS: u16 = 0x8000;

    fn new() -> Self {
        Self { entries: vec![(u32::MAX, 0, 0); 1 << Self::BITS] }
    }

    fn slot(addr: u64) -> usize {
        ((addr as u32 >> 1) & ((1 << Self::BITS) - 1)) as usize
    }

    fn get(&self, addr: u64, size: u32) -> Option<BlockInfo> {
        let (a, s, n) = self.entries[Self::slot(addr)];
        (a == addr as u32 && u32::from(s) == size).then_some(BlockInfo {
            insns: u32::from(n & 0x3FFF),
            stores: n & Self::STORES != 0,
            side_effects: n & Self::SIDE_EFFECTS != 0,
        })
    }

    fn insert(&mut self, addr: u64, size: u32, info: BlockInfo) {
        let mut n = info.insns.min(0x3FFF) as u16;
        if info.stores {
            n |= Self::STORES;
        }
        if info.side_effects {
            n |= Self::SIDE_EFFECTS;
        }
        self.entries[Self::slot(addr)] = (addr as u32, size as u16, n);
    }

    fn clear(&mut self) {
        self.entries.fill((u32::MAX, 0, 0));
    }
}

/// Decodes a block: counts its Thumb instructions and conservatively
/// classifies instructions with effects beyond the general-purpose registers.
fn analyze_block(code: &[u8]) -> BlockInfo {
    let mut i = 0;
    let mut info = BlockInfo { insns: 0, stores: false, side_effects: false };
    while i + 1 < code.len() {
        let hw = u16::from_le_bytes([code[i], code[i + 1]]);
        let wide = hw >> 11 >= 0b11101;
        if wide {
            info.stores |= thumb32_store(hw);
            info.side_effects |= thumb32_side_effects(hw);
        } else {
            info.stores |= thumb16_store(hw);
            info.side_effects |= thumb16_side_effects(hw);
        }
        i += if wide { 4 } else { 2 };
        info.insns += 1;
    }
    info
}

fn thumb16_store(hw: u16) -> bool {
    matches!(hw & 0xFE00, 0x5000 | 0x5200 | 0x5400) // STR/STRH/STRB (register)
        || matches!(hw & 0xF800, 0x6000 | 0x7000 | 0x8000 | 0x9000 | 0xC000) // STR/STRB/STRH imm, STR SP, STM
        || hw & 0xFE00 == 0xB400 // PUSH
}

fn thumb16_side_effects(hw: u16) -> bool {
    hw & 0xFFE0 == 0xB660 // CPSIE/CPSID
        || hw & 0xFF00 == 0xDF00 // SVC
}

fn thumb32_store(hw1: u16) -> bool {
    (hw1 & 0xFE00 == 0xE800 && hw1 & 0x0010 == 0) // STM/STMDB/PUSH.W, STRD/STREX
        || hw1 & 0xFF10 == 0xF800 // STR/STRB/STRH (wide)
        || hw1 & 0xEE10 == 0xEC00 // VSTR/VSTM/VPUSH, STC, MCRR
}

fn thumb32_side_effects(hw1: u16) -> bool {
    hw1 & 0xEE00 == 0xEC00 // coprocessor / VFP loads and stores (FP registers)
        || hw1 & 0xEF00 == 0xEE00 // coprocessor/VFP data ops incl. VMSR
        || hw1 & 0xFFE0 == 0xF380 // MSR
}

/// Detects loops that cannot make progress until a timed event happens.
///
/// The machine is deterministic. If, at the head of a loop, registers and RAM
/// are identical to an earlier visit, and nothing in between could have been
/// influenced from outside (no peripheral write, no time-dependent peripheral
/// read, no exception, no state outside registers/RAM changed), the loop
/// repeats this cycle forever. Virtual time can then jump to the next event.
/// Typical: `while (HAL_GetTick() - t < d);` or polling unchanged inputs.
#[derive(Default)]
struct IdleDetector {
    head: u64,
    blocks_since_head: u32,
    prev_block: u64,
    /// Snapshot taken at a visit of `head`.
    snapshot: Option<Snapshot>,
    /// Nothing outside registers/RAM happened since the snapshot.
    clean: bool,
    /// RAM may have been written since the snapshot.
    stored: bool,
    /// Earliest cycle at which a peripheral read since the snapshot may
    /// return a different value (see `Peripheral::read_stable_until`).
    stable_until: u64,
    /// Visits of `head` to skip before comparing again (back-off).
    skip: u32,
    backoff: u32,
    /// Total cycles skipped (statistics).
    skipped_cycles: u64,
}

struct Snapshot {
    /// Cycle count when the snapshot was taken.
    at: u64,
    regs: [u32; IDLE_REGS.len()],
    /// RAM contents, if stores happened in the previous period.
    ram: Option<Vec<u8>>,
}

const IDLE_REGS: [RegisterARM; 21] = [
    RegisterARM::R0,
    RegisterARM::R1,
    RegisterARM::R2,
    RegisterARM::R3,
    RegisterARM::R4,
    RegisterARM::R5,
    RegisterARM::R6,
    RegisterARM::R7,
    RegisterARM::R8,
    RegisterARM::R9,
    RegisterARM::R10,
    RegisterARM::R11,
    RegisterARM::R12,
    RegisterARM::SP,
    RegisterARM::LR,
    RegisterARM::XPSR,
    RegisterARM::FPSCR,
    RegisterARM::PRIMASK,
    RegisterARM::BASEPRI,
    RegisterARM::FAULTMASK,
    RegisterARM::CONTROL,
];

impl IdleDetector {
    /// Something outside registers/RAM happened (MMIO, exception, ...).
    fn dirty(&mut self) {
        self.clean = false;
    }

    /// A peripheral read at `now` returned a value that stays the same for
    /// reads before cycle `until`.
    fn note_read(&mut self, until: u64, now: u64) {
        if until <= now {
            self.dirty();
        } else {
            self.stable_until = self.stable_until.min(until);
        }
    }

    /// Starts a new observation period.
    fn restart(&mut self) {
        self.snapshot = None;
        self.clean = true;
        self.stored = false;
        self.stable_until = u64::MAX;
    }

    fn read_regs(uc: &Unicorn<System>) -> [u32; IDLE_REGS.len()] {
        let mut r = [0u32; IDLE_REGS.len()];
        for (v, reg) in r.iter_mut().zip(IDLE_REGS) {
            *v = uc.reg_read(reg).unwrap_or(0) as u32;
        }
        r
    }

    fn read_ram(uc: &Unicorn<System>) -> Vec<u8> {
        let ram = &uc.get_data().config.memory.ram;
        let mut buf = vec![0u8; ram.iter().map(|&(_, size)| size as usize).sum()];
        let mut off = 0;
        for &(base, size) in ram {
            let _ = uc.mem_read(u64::from(base), &mut buf[off..off + size as usize]);
            off += size as usize;
        }
        buf
    }

    fn take_snapshot(uc: &mut Unicorn<System>, with_ram: bool) {
        let regs = Self::read_regs(uc);
        let ram = with_ram.then(|| Self::read_ram(uc));
        let at = uc.get_data().clock.cycles();
        let d = &mut uc.get_data_mut().idle;
        d.snapshot = Some(Snapshot { at, regs, ram });
        d.clean = true;
        d.stored = false;
        d.stable_until = u64::MAX;
    }

    /// Called at the start of every block. Returns `Some(t)` if the core
    /// repeats the same loop iteration until cycle `t` (`u64::MAX`: until the
    /// next event).
    fn on_block(uc: &mut Unicorn<System>, addr: u64, info: BlockInfo) -> Option<u64> {
        enum Action {
            Nothing,
            Snapshot { with_ram: bool },
            Compare,
        }
        let action = {
            let d = &mut uc.get_data_mut().idle;
            let prev = std::mem::replace(&mut d.prev_block, addr);
            if addr != d.head {
                d.blocks_since_head += 1;
                // A backward branch starts a new candidate unless we are still
                // inside the body of the current one.
                if addr <= prev && d.blocks_since_head > 64 {
                    d.head = addr;
                    d.blocks_since_head = 0;
                    d.restart();
                    d.skip = 0;
                    d.backoff = 0;
                }
                Action::Nothing
            } else {
                d.blocks_since_head = 0;
                if !d.clean {
                    // Outside influence: start over, but only snapshot after a
                    // clean period so that busy loops pay nothing.
                    d.restart();
                    Action::Nothing
                } else if d.snapshot.is_none() {
                    Action::Snapshot { with_ram: d.stored }
                } else if d.skip > 0 {
                    d.skip -= 1;
                    Action::Nothing
                } else {
                    Action::Compare
                }
            }
        };
        let idle = match action {
            Action::Nothing => None,
            Action::Snapshot { with_ram } => {
                Self::take_snapshot(uc, with_ram);
                None
            }
            Action::Compare => {
                let regs = Self::read_regs(uc);
                let d = &uc.get_data().idle;
                let snap = d.snapshot.as_ref().expect("snapshot present");
                let same = regs == snap.regs
                    && match (&snap.ram, d.stored) {
                        (_, false) => true,
                        (Some(ram), true) => *ram == Self::read_ram(uc),
                        (None, true) => false,
                    };
                if !same {
                    let with_ram = d.stored;
                    Self::take_snapshot(uc, with_ram);
                    // The loop makes progress (e.g. a counting delay loop):
                    // compare less and less often.
                    let d = &mut uc.get_data_mut().idle;
                    d.backoff = (d.backoff * 2).clamp(1, 4096);
                    d.skip = d.backoff;
                    None
                } else if d.stable_until == u64::MAX {
                    Some(u64::MAX)
                } else {
                    // Reads return the same values only before `stable_until`:
                    // repeat whole iterations (same timing as without
                    // skipping) as long as all of their reads happen before it
                    // and before the slice ends.
                    let (at, with_ram) = (snap.at, d.stored);
                    let until = d.stable_until.min(uc.get_data().slice_end);
                    let now = uc.get_data().clock.cycles();
                    let period = now - at;
                    if period == 0 || until <= now {
                        None
                    } else {
                        // The next period starts here; its reads are noted afresh.
                        Self::take_snapshot(uc, with_ram);
                        Some(now + (until - now) / period * period)
                    }
                }
            }
        };
        // The current block belongs to the next period.
        let d = &mut uc.get_data_mut().idle;
        if info.side_effects {
            d.clean = false;
        }
        if info.stores {
            d.stored = true;
        }
        idle
    }
}

const EXC_RETURN_MAGIC: u32 = 0xFFFF_FF00;

// QEMU exception numbers reported through the interrupt hook.
const EXCP_UDEF: u32 = 1;
const EXCP_SWI: u32 = 2;
const EXCP_PREFETCH_ABORT: u32 = 3;
const EXCP_DATA_ABORT: u32 = 4;
const EXCP_BKPT: u32 = 7;
const EXCP_EXCEPTION_EXIT: u32 = 8;
const EXCP_NOCP: u32 = 17;
const EXCP_INVSTATE: u32 = 18;
const EXCP_UNALIGNED: u32 = 22;

impl Machine {
    pub fn new(config: MachineConfig, bus: Bus) -> Result<Self, MachineError> {
        let system = System {
            clock: Clock::new(config.reset_cpu_hz),
            nvic: Nvic::new(),
            scs: Scs::new(),
            bus,
            config: config.clone(),
            slice_end: 0,
            block_insns: BlockCache::new(),
            last_invalid: None,
            stop: None,
            reset_requested: false,
            idle: IdleDetector::default(),
            idle_skipping: true,
            stepping: false,
            watch_hit: None,
        };
        let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::THUMB | Mode::MCLASS, system)?;
        uc.ctl_set_cpu_model(ArmCpuModel::CORTEX_M4 as i32)?;
        // Breakpoints are exits; `emu_start`'s `until` is not used.
        uc.ctl_exits_enable()?;
        uc.ctl_set_exits(&[])?;

        let mem = &config.memory;
        let mut flash = vec![0xFFu8; mem.flash_size as usize].into_boxed_slice();
        let flash_ptr = flash.as_mut_ptr().cast();
        // SAFETY: `flash` is owned by the machine, never reallocated and outlives `uc`.
        unsafe {
            uc.mem_map_ptr(u64::from(mem.flash_base), u64::from(mem.flash_size), Prot::READ | Prot::EXEC, flash_ptr)?;
            if mem.flash_alias_at_zero {
                uc.mem_map_ptr(0, u64::from(mem.flash_size), Prot::READ | Prot::EXEC, flash_ptr)?;
            }
        }
        for &(base, size) in &mem.ram {
            uc.mem_map(u64::from(base), u64::from(size), Prot::ALL)?;
        }
        for &(base, size) in &mem.rom_zero {
            uc.mem_map(u64::from(base), u64::from(size), Prot::READ)?;
        }

        uc.mmio_map(
            u64::from(mem.periph_base),
            u64::from(mem.periph_size),
            Some(move |uc: &mut Unicorn<System>, off: u64, size: usize| {
                let sys = uc.get_data_mut();
                let addr = sys.config.memory.periph_base + off as u32;
                let (mut ctx, bus) = sys.ctx();
                let value = bus.read(&mut ctx, addr, size as u32);
                let now = sys.clock.cycles();
                let until = sys.bus.read_stable_until(addr, &sys.clock);
                sys.idle.note_read(until, now);
                u64::from(value)
            }),
            Some(move |uc: &mut Unicorn<System>, off: u64, size: usize, value: u64| {
                let sys = uc.get_data_mut();
                sys.idle.dirty();
                let addr = sys.config.memory.periph_base + off as u32;
                let (mut ctx, bus) = sys.ctx();
                bus.write(&mut ctx, addr, size as u32, value as u32);
                // A device may have raised an interrupt: let the run loop look at it.
                if sys.nvic.any_pending() {
                    sys.request_stop();
                }
            }),
        )?;

        uc.mmio_map(
            u64::from(PPB_BASE),
            u64::from(PPB_SIZE),
            Some(|uc: &mut Unicorn<System>, off: u64, size: usize| {
                let ipsr = uc.reg_read(RegisterARM::IPSR).unwrap_or(0) as u32;
                let sys = uc.get_data_mut();
                let now = sys.clock.cycles();
                let until = sys.scs.read_stable_until(off as u32, now);
                sys.idle.note_read(until, now);
                u64::from(sys.scs.read(&sys.nvic, now, ipsr, off as u32, size as u32))
            }),
            Some(|uc: &mut Unicorn<System>, off: u64, size: usize, value: u64| {
                let sys = uc.get_data_mut();
                sys.idle.dirty();
                let now = sys.clock.cycles();
                match sys.scs.write(&mut sys.nvic, now, off as u32, size as u32, value as u32) {
                    ScsAction::None => {}
                    ScsAction::CheckPending => sys.request_stop(),
                    ScsAction::SystemReset => {
                        sys.reset_requested = true;
                        sys.request_stop();
                    }
                }
            }),
        )?;

        if let Some((region, alias)) = mem.sram_bitband {
            // Bit-band alias: each word maps to one bit of the SRAM region.
            let target = move |off: u64| (region + (off / 32) as u32, ((off / 4) % 8) as u32);
            uc.mmio_map(
                u64::from(alias),
                0x0200_0000,
                Some(move |uc: &mut Unicorn<System>, off: u64, _size: usize| {
                    let (byte, bit) = target(off);
                    let mut b = [0u8];
                    let _ = uc.mem_read(u64::from(byte), &mut b);
                    u64::from((b[0] >> bit) & 1)
                }),
                Some(move |uc: &mut Unicorn<System>, off: u64, _size: usize, value: u64| {
                    let (byte, bit) = target(off);
                    let mut b = [0u8];
                    if uc.mem_read(u64::from(byte), &mut b).is_ok() {
                        let new = if value & 1 != 0 { b[0] | (1 << bit) } else { b[0] & !(1 << bit) };
                        let _ = uc.mem_write(u64::from(byte), &[new]);
                    }
                }),
            )?;
        }

        // Time keeping: account for each executed block and end the slice.
        // This is the only place that calls `emu_stop`: at a block boundary no
        // instruction is half-done, so execution can resume at the current PC.
        uc.add_block_hook(1, 0, |uc, addr, size| {
            if uc.get_data().stepping {
                // `step` accounts for its single instruction. (With a branch the
                // hook also fires for the next block before the step ends.)
                return;
            }
            if uc.get_data().clock.cycles() >= uc.get_data().slice_end {
                let _ = uc.emu_stop();
                return;
            }
            let info = match uc.get_data().block_insns.get(addr, size) {
                Some(info) => info,
                None => {
                    let mut code = vec![0u8; size as usize];
                    let info = if uc.mem_read(addr, &mut code).is_ok() {
                        analyze_block(&code)
                    } else {
                        BlockInfo { insns: 1, stores: true, side_effects: true }
                    };
                    uc.get_data_mut().block_insns.insert(addr, size, info);
                    info
                }
            };
            let idle_until = if uc.get_data().idle_skipping { IdleDetector::on_block(uc, addr, info) } else { None };
            if let Some(until) = idle_until {
                let sys = uc.get_data_mut();
                let now = sys.clock.cycles();
                if until >= sys.slice_end {
                    // Nothing can change before the next event: jump to the end of the slice.
                    if sys.slice_end > now {
                        sys.idle.skipped_cycles += sys.slice_end - now;
                        sys.clock.advance(sys.slice_end - now);
                    }
                    let _ = uc.emu_stop();
                    return;
                }
                // A polled register changes before the slice ends: skip the
                // iterations up to that point and continue with this block.
                sys.idle.skipped_cycles += until - now;
                sys.clock.advance(until - now);
            }
            uc.get_data_mut().clock.advance(u64::from(info.insns));
        })?;

        uc.add_intr_hook(Self::on_cpu_exception)?;

        let invalid = HookType(
            HookType::MEM_READ_UNMAPPED.0
                | HookType::MEM_WRITE_UNMAPPED.0
                | HookType::MEM_FETCH_UNMAPPED.0
                | HookType::MEM_WRITE_PROT.0
                | HookType::MEM_FETCH_PROT.0,
        );
        uc.add_mem_hook(invalid, 1, 0, |uc, ty, addr, _size, _value| {
            let is_write = matches!(ty, MemType::WRITE_UNMAPPED | MemType::WRITE_PROT);
            let is_fetch = matches!(ty, MemType::FETCH_UNMAPPED | MemType::FETCH_PROT);
            uc.get_data_mut().last_invalid = Some((addr as u32, is_write, is_fetch));
            false
        })?;

        Ok(Self {
            uc,
            flash,
            breakpoints: BTreeSet::new(),
            watchpoints: Vec::new(),
            watch_stopper: None,
            #[cfg(feature = "gdb")]
            gdb_resume: None,
        })
    }

    pub fn system(&self) -> &System {
        self.uc.get_data()
    }

    pub fn system_mut(&mut self) -> &mut System {
        self.uc.get_data_mut()
    }

    pub fn clock(&self) -> &Clock {
        &self.uc.get_data().clock
    }

    pub fn reg(&self, r: CoreReg) -> u32 {
        self.uc.reg_read(uc_reg(r)).unwrap_or(0) as u32
    }

    pub fn set_reg(&mut self, r: CoreReg, v: u32) {
        UcCore(&mut self.uc).set_reg(r, v);
    }

    pub fn pc(&self) -> u32 {
        self.reg(CoreReg::Pc)
    }

    pub fn read_mem(&self, addr: u32, buf: &mut [u8]) -> bool {
        self.uc.mem_read(u64::from(addr), buf).is_ok()
    }

    pub fn read_u32(&self, addr: u32) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read_mem(addr, &mut b).then(|| u32::from_le_bytes(b))
    }

    /// Writes to memory, including flash (as a debugger/programmer would).
    pub fn write_mem(&mut self, addr: u32, data: &[u8]) -> bool {
        let mem = &self.uc.get_data().config.memory;
        let fb = mem.flash_base;
        let in_flash =
            |a: u32| a >= fb && u64::from(a) + data.len() as u64 <= u64::from(fb) + u64::from(mem.flash_size);
        let ok = if in_flash(addr) {
            let off = (addr - fb) as usize;
            self.flash[off..off + data.len()].copy_from_slice(data);
            true
        } else {
            self.uc.mem_write(u64::from(addr), data).is_ok()
        };
        if ok {
            // Drop stale translations of modified code.
            let _ = self.uc.ctl_remove_cache(u64::from(addr), u64::from(addr) + data.len() as u64);
            self.uc.get_data_mut().block_insns.clear();
        }
        ok
    }

    /// Loads all segments of an ELF image into memory (like flashing it).
    pub fn load_elf(&mut self, image: &ElfImage) -> Result<(), MachineError> {
        for seg in &image.segments {
            if !self.write_mem(seg.addr, &seg.data) {
                return Err(MachineError::SegmentOutOfRange { addr: seg.addr, len: seg.data.len() });
            }
        }
        Ok(())
    }

    /// Loads a raw binary at `addr` (e.g. the font table at 0x0810_0000).
    pub fn load_bin(&mut self, addr: u32, data: &[u8]) -> Result<(), MachineError> {
        if self.write_mem(addr, data) { Ok(()) } else { Err(MachineError::SegmentOutOfRange { addr, len: data.len() }) }
    }

    /// Resets the core and the system peripherals (memory contents are kept).
    pub fn reset(&mut self) {
        let sys = self.uc.get_data_mut();
        sys.nvic.reset();
        sys.scs.reset();
        sys.bus.reset();
        sys.clock.set_cpu_hz(sys.config.reset_cpu_hz);
        sys.stop = None;
        sys.watch_hit = None;
        sys.reset_requested = false;
        sys.last_invalid = None;
        sys.idle = IdleDetector::default();

        let msp = self.read_u32(0).unwrap_or(0);
        let reset_vector = self.read_u32(4).unwrap_or(0);
        let mut core = UcCore(&mut self.uc);
        core.set_reg(CoreReg::Xpsr, 1 << 24);
        core.set_reg(CoreReg::Control, 0);
        core.set_reg(CoreReg::Primask, 0);
        core.set_reg(CoreReg::Faultmask, 0);
        core.set_reg(CoreReg::Basepri, 0);
        core.set_reg(CoreReg::Psp, 0);
        core.set_reg(CoreReg::Msp, msp & !3);
        core.set_reg(CoreReg::Lr, 0xFFFF_FFFF);
        core.set_reg(CoreReg::Pc, reset_vector | 1);
    }

    /// Stops execution before the instruction at `addr` (debugger breakpoint).
    pub fn add_breakpoint(&mut self, addr: u32) {
        if self.breakpoints.insert(addr & !1) {
            self.update_exits(addr & !1);
        }
    }

    pub fn remove_breakpoint(&mut self, addr: u32) {
        if self.breakpoints.remove(&(addr & !1)) {
            self.update_exits(addr & !1);
        }
    }

    pub fn breakpoints(&self) -> impl Iterator<Item = u32> + '_ {
        self.breakpoints.iter().copied()
    }

    /// Stops execution after an access of `kind` to `addr..addr + len`.
    pub fn add_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> bool {
        let hook_type = match kind {
            WatchKind::Write => HookType::MEM_WRITE,
            WatchKind::Read => HookType::MEM_READ,
            WatchKind::Access => HookType(HookType::MEM_READ.0 | HookType::MEM_WRITE.0),
        };
        let end = u64::from(addr) + u64::from(len.max(1)) - 1;
        let hook = self.uc.add_mem_hook(hook_type, u64::from(addr), end, move |uc, _ty, _addr, _size, _value| {
            let sys = uc.get_data_mut();
            sys.watch_hit.get_or_insert((addr, kind));
            // Not `emu_stop`: the access must complete.
            sys.request_stop();
            true
        });
        let Ok(hook) = hook else { return false };
        self.watchpoints.push(Watchpoint { addr, len, kind, hook });
        if self.watch_stopper.is_none() {
            // Unicorn checks for a stop request after each code hook, i.e.
            // before the next instruction (a block would otherwise run to its end).
            self.watch_stopper = self
                .uc
                .add_code_hook(1, 0, |uc, _addr, _size| {
                    if uc.get_data().watch_hit.is_some() {
                        let _ = uc.emu_stop();
                    }
                })
                .ok();
        }
        // Retranslate so that existing code calls the new hooks.
        let _ = self.uc.ctl_flush_tb();
        true
    }

    fn remove_watch_stopper_if_unused(&mut self) {
        if self.watchpoints.is_empty()
            && let Some(h) = self.watch_stopper.take()
        {
            let _ = self.uc.remove_hook(h);
        }
    }

    pub fn remove_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> bool {
        let Some(i) = self.watchpoints.iter().position(|w| w.addr == addr && w.len == len && w.kind == kind) else {
            return false;
        };
        let w = self.watchpoints.remove(i);
        let _ = self.uc.remove_hook(w.hook);
        self.remove_watch_stopper_if_unused();
        let _ = self.uc.ctl_flush_tb();
        true
    }

    /// Removes all breakpoints and watchpoints.
    pub fn clear_debug_points(&mut self) {
        for addr in self.breakpoints().collect::<Vec<_>>() {
            self.remove_breakpoint(addr);
        }
        for w in std::mem::take(&mut self.watchpoints) {
            let _ = self.uc.remove_hook(w.hook);
        }
        self.remove_watch_stopper_if_unused();
        let _ = self.uc.ctl_flush_tb();
    }

    fn take_watch_hit(&mut self) -> Option<StopReason> {
        let (addr, kind) = self.uc.get_data_mut().watch_hit.take()?;
        Some(StopReason::Watchpoint { pc: self.pc(), addr, kind })
    }

    fn update_exits(&mut self, changed: u32) {
        let exits: Vec<u64> = self.breakpoints.iter().map(|&a| u64::from(a)).collect();
        let _ = self.uc.ctl_set_exits(&exits);
        // Exits are checked when code is translated: retranslate around `changed`.
        let _ = self.uc.ctl_remove_cache(u64::from(changed), u64::from(changed) + 4);
    }

    /// Executes exactly one instruction (also one at a breakpoint). Pending
    /// interrupts are not taken, like a debugger stepping with interrupts
    /// masked; exceptions caused by the instruction itself (SVC, faults) are.
    pub fn step(&mut self) -> StopReason {
        self.sync_devices();
        if self.uc.get_data().reset_requested {
            self.reset();
        }
        let pc = self.pc();
        let at_breakpoint = self.breakpoints.remove(&pc);
        if at_breakpoint {
            self.update_exits(pc);
        }
        let sys = self.uc.get_data_mut();
        sys.stepping = true;
        sys.slice_end = u64::MAX;
        let result = self.uc.emu_start(u64::from(pc | 1), u64::MAX, 0, 1);
        let sys = self.uc.get_data_mut();
        sys.stepping = false;
        sys.clock.advance(1);
        if at_breakpoint {
            self.add_breakpoint(pc);
        }
        if let Err(err) = result {
            self.on_emu_error(err);
        }
        let stop = self.uc.get_data_mut().stop.take();
        stop.or_else(|| self.take_watch_hit()).unwrap_or(StopReason::Step)
    }

    /// Runs until `budget` more cycles have been executed or the core stops.
    /// A breakpoint at the current PC is stepped over (resuming from it).
    pub fn run(&mut self, budget: u64) -> StopReason {
        let end = self.clock().cycles().saturating_add(budget);
        let resume_pc = self.breakpoints.contains(&self.pc()).then(|| self.pc());
        let mut first = true;
        loop {
            self.sync_devices();
            if self.uc.get_data().reset_requested {
                self.reset();
            }
            if let Some(stop) = self.uc.get_data_mut().stop.take() {
                return stop;
            }
            if let Some(stop) = self.take_watch_hit() {
                return stop;
            }
            self.take_pending_exception();
            if let Some(stop) = self.uc.get_data_mut().stop.take() {
                return stop;
            }

            let sys = self.uc.get_data();
            let now = sys.clock.cycles();
            if now >= end {
                return StopReason::Budget;
            }
            let mut slice_end = end.min(now + sys.config.quantum);
            if sys.nvic.any_pending() {
                slice_end = slice_end.min(now + sys.config.masked_quantum);
            }
            let next_event = [sys.scs.systick.next_event(), sys.bus.next_event()].into_iter().flatten().min();
            if let Some(ev) = next_event {
                slice_end = slice_end.min(ev.max(now + 1));
            }
            self.uc.get_data_mut().slice_end = slice_end;

            let pc = self.pc();
            if std::mem::take(&mut first) && resume_pc == Some(pc) {
                // Unicorn would stop right away at the exit.
                match self.step() {
                    StopReason::Step => continue,
                    other => return other,
                }
            }
            let result = self.uc.emu_start(u64::from(pc | 1), u64::MAX, 0, 0);
            if let Err(err) = result {
                self.on_emu_error(err);
            } else if self.breakpoints.contains(&self.pc()) && self.uc.get_data().stop.is_none() {
                return StopReason::DebugBreakpoint { pc: self.pc() };
            } else if self.clock().cycles() < slice_end && !self.stopped_on_purpose() {
                self.on_idle_return();
            }
        }
    }

    fn stopped_on_purpose(&self) -> bool {
        let sys = self.uc.get_data();
        sys.stop.is_some() || sys.watch_hit.is_some() || sys.reset_requested || sys.nvic.any_pending()
    }

    /// Unicorn returned early without an error: either we stopped it from a
    /// callback (handled by the loop) or the core executed WFI/WFE.
    fn on_idle_return(&mut self) {
        let pc = self.pc();
        let mut prev = [0u8; 2];
        let is_wait =
            self.read_mem(pc.wrapping_sub(2), &mut prev) && matches!(u16::from_le_bytes(prev), 0xBF30 | 0xBF20); // WFI / WFE
        if is_wait {
            // Sleep until the next timed event.
            let sys = self.uc.get_data_mut();
            let next = [sys.scs.systick.next_event(), sys.bus.next_event()].into_iter().flatten().min();
            match next {
                Some(t) => {
                    let now = sys.clock.cycles();
                    sys.clock.advance(t.saturating_sub(now).max(1));
                }
                None => {
                    sys.stop = Some(StopReason::Lockup { pc, reason: "WFI with no wake-up source".into() });
                }
            }
        }
    }

    fn sync_devices(&mut self) {
        let sys = self.uc.get_data_mut();
        let now = sys.clock.cycles();
        if sys.scs.systick.update(now) {
            sys.nvic.set_pending(nvic::SYSTICK);
        }
        let (mut ctx, bus) = sys.ctx();
        bus.update(&mut ctx);
    }

    fn take_pending_exception(&mut self) {
        Self::take_pending(&mut self.uc);
    }

    fn take_pending(uc: &mut Unicorn<System>) {
        let masks = UcCore(uc).masks();
        let Some(exc) = uc.get_data().nvic.pending_to_take(masks) else { return };
        let return_addr = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
        Self::enter_exception(uc, exc, return_addr);
    }

    fn enter_exception(uc: &mut Unicorn<System>, exc: u32, return_addr: u32) {
        uc.get_data_mut().idle.dirty();
        let (vtor, stkalign) = {
            let s = &uc.get_data().scs;
            (s.vtor, s.stkalign())
        };
        // `enter` needs the core and the NVIC at the same time; work on a copy of the NVIC.
        let mut nvic = uc.get_data().nvic.clone();
        let res = exception::enter(&mut UcCore(uc), &mut nvic, exc, return_addr, vtor, stkalign);
        uc.get_data_mut().nvic = nvic;
        if let Err(e) = res {
            Self::lockup(uc, format!("exception entry for exception {exc} failed: {e}"));
        }
    }

    fn lockup(uc: &mut Unicorn<System>, reason: String) {
        let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
        log::error!("core lockup at {pc:#010x}: {reason}");
        let sys = uc.get_data_mut();
        sys.stop = Some(StopReason::Lockup { pc, reason });
        sys.request_stop();
    }

    /// Raises a synchronous fault; `return_addr` is the faulting instruction.
    fn raise_fault(uc: &mut Unicorn<System>, exc: u32, return_addr: u32) {
        let masks = UcCore(uc).masks();
        let target = uc.get_data().nvic.route_fault(exc, masks);
        match target {
            Some(target) => {
                if target == nvic::HARD_FAULT && exc != nvic::HARD_FAULT {
                    uc.get_data_mut().scs.hfsr |= scs::HFSR_FORCED;
                }
                log::debug!("fault: exception {exc} -> {target} at {return_addr:#010x}");
                uc.get_data_mut().nvic.set_pending(target);
                Self::enter_exception(uc, target, return_addr);
            }
            None => Self::lockup(uc, format!("fault (exception {exc}) while in HardFault/NMI or with FAULTMASK set")),
        }
    }

    fn on_cpu_exception(uc: &mut Unicorn<System>, intno: u32) {
        uc.get_data_mut().idle.dirty();
        let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
        match intno {
            EXCP_EXCEPTION_EXIT => {
                let mut nvic = uc.get_data().nvic.clone();
                let res = exception::exit(&mut UcCore(uc), &mut nvic, pc | EXC_RETURN_MAGIC);
                uc.get_data_mut().nvic = nvic;
                match res {
                    Ok(()) => Self::take_pending(uc), // tail-chaining
                    Err(ExceptionError::InvalidExcReturn(_)) => {
                        uc.get_data_mut().scs.cfsr |= scs::CFSR_INVSTATE;
                        Self::raise_fault(uc, nvic::USAGE_FAULT, pc);
                    }
                    Err(e) => Self::lockup(uc, format!("exception return failed: {e}")),
                }
            }
            EXCP_SWI => {
                // PC already points behind the SVC instruction.
                let masks = UcCore(uc).masks();
                let sys = uc.get_data_mut();
                sys.nvic.set_pending(nvic::SVCALL);
                if sys.nvic.pending_to_take(masks) == Some(nvic::SVCALL) {
                    Self::enter_exception(uc, nvic::SVCALL, pc);
                } else {
                    uc.get_data_mut().nvic.clear_pending(nvic::SVCALL);
                    Self::raise_fault(uc, nvic::HARD_FAULT, pc);
                }
            }
            EXCP_BKPT => {
                let sys = uc.get_data_mut();
                sys.stop = Some(StopReason::Breakpoint { pc });
                sys.request_stop();
            }
            EXCP_UDEF | EXCP_NOCP | EXCP_INVSTATE | EXCP_UNALIGNED => {
                let bit = match intno {
                    EXCP_UDEF => scs::CFSR_UNDEFINSTR,
                    EXCP_INVSTATE => scs::CFSR_INVSTATE,
                    EXCP_UNALIGNED => scs::CFSR_UNALIGNED,
                    _ => 1 << 19, // NOCP
                };
                uc.get_data_mut().scs.cfsr |= bit;
                Self::raise_fault(uc, nvic::USAGE_FAULT, pc);
            }
            EXCP_PREFETCH_ABORT | EXCP_DATA_ABORT => {
                uc.get_data_mut().scs.cfsr |= scs::CFSR_PRECISERR;
                Self::raise_fault(uc, nvic::BUS_FAULT, pc);
            }
            other => {
                log::warn!("unhandled CPU exception {other} at {pc:#010x}");
                Self::raise_fault(uc, nvic::HARD_FAULT, pc);
            }
        }
    }

    /// Unicorn aborted because of an invalid memory access or instruction.
    fn on_emu_error(&mut self, err: uc_error) {
        let pc = self.pc();
        let invalid = self.uc.get_data_mut().last_invalid.take();
        match (err, invalid) {
            (uc_error::INSN_INVALID, _) => {
                self.uc.get_data_mut().scs.cfsr |= scs::CFSR_UNDEFINSTR;
                Self::raise_fault(&mut self.uc, nvic::USAGE_FAULT, pc);
            }
            (_, Some((addr, _is_write, is_fetch))) => {
                let sys = self.uc.get_data_mut();
                if is_fetch {
                    sys.scs.cfsr |= scs::CFSR_IBUSERR;
                } else {
                    sys.scs.cfsr |= scs::CFSR_PRECISERR | scs::CFSR_BFARVALID;
                    sys.scs.bfar = addr;
                }
                log::debug!("bus fault: access to {addr:#010x} at pc {pc:#010x}");
                Self::raise_fault(&mut self.uc, nvic::BUS_FAULT, pc);
            }
            (err, None) => {
                Self::lockup(&mut self.uc, format!("emulator error {err:?}"));
            }
        }
    }
}
