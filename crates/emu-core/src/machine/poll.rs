//! Polling loops run in place of the translated code.
//!
//! A loop polling a counter (`while (timer_hw->timerawl < t);`, the Pico
//! SDK's `busy_wait_until`) changes state with every counter tick, so idle
//! detection skips it only up to the next tick, and running it through
//! Unicorn with the block hook costs more than the time it covers. Such
//! loops only load and compare. [`Machine::fast_forward_poll`] runs them
//! here, block by block with the block hook's timekeeping and the MMIO
//! callback's peripheral reads, and skips whole iterations while the
//! registers the loop reads before writing and the polled values repeat.
//! Cycles and accesses (and their times) are the same as running the loop.

use super::{Machine, RAM_PAGE, System, uc_its_core_regs, uc_its_set_core_regs};
use crate::machine::CpuModel;

/// Blocks of a loop that are run here (more: Unicorn runs it).
const MAX_BLOCKS: usize = 8;
const MAX_BLOCK_INSNS: u32 = 32;

/// Flag bits in register masks (bit n: Rn).
const N: u32 = 1 << 16;
const Z: u32 = 1 << 17;
const C: u32 = 1 << 18;
const V: u32 = 1 << 19;
const NZCV: u32 = N | Z | C | V;
const ALL: u32 = 0xFFFF | NZCV;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operand {
    Imm(u32),
    Reg(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addr {
    /// `[rn, #off]`
    Imm { rn: u8, off: u32 },
    /// `[rn, rm]`
    Reg { rn: u8, rm: u8 },
    /// Literal pool (PC-relative).
    Abs(u32),
}

/// A 16-bit Thumb instruction of the subset polling loops use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// LSLS/LSRS/ASRS Rd, Rm, #imm (`kind` 0, 1, 2); LSLS #0 is MOVS Rd, Rm.
    ShiftImm {
        kind: u8,
        rd: u8,
        rm: u8,
        imm: u8,
    },
    /// ADDS/SUBS Rd, Rn, #imm / Rm.
    AddSub {
        rd: u8,
        rn: u8,
        b: Operand,
        sub: bool,
    },
    MovImm {
        rd: u8,
        imm: u32,
    },
    CmpImm {
        rn: u8,
        imm: u32,
    },
    /// Data processing with registers (ANDS ... MVNS), `op` 0..15.
    Alu {
        op: u8,
        rdn: u8,
        rm: u8,
    },
    /// ADD / CMP / MOV with high registers (not the PC).
    AddHi {
        rd: u8,
        rm: u8,
    },
    CmpHi {
        rn: u8,
        rm: u8,
    },
    MovHi {
        rd: u8,
        rm: u8,
    },
    /// ADD Rd, SP, #imm; ADR (`base` 13 / none); ADD/SUB SP, #imm.
    AddSp {
        rd: u8,
        imm: u32,
    },
    Adr {
        rd: u8,
        value: u32,
    },
    AdjustSp {
        delta: u32,
    },
    /// SXTH, SXTB, UXTH, UXTB (`kind` 0..3).
    Extend {
        kind: u8,
        rd: u8,
        rm: u8,
    },
    Load {
        rt: u8,
        addr: Addr,
        size: u8,
        signed: bool,
    },
    Nop,
}

/// How a block ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    /// The translator ends blocks before the next page.
    Fall,
    Jump(u32),
    Cond {
        cond: u8,
        target: u32,
    },
    Cbz {
        rn: u8,
        nonzero: bool,
        target: u32,
    },
}

#[derive(Debug)]
struct Block {
    addr: u32,
    size: u32,
    ops: Vec<Op>,
    end: End,
    /// Registers and flags read before they are written / written.
    uses: u32,
    defs: u32,
}

impl Block {
    fn next(&self) -> u32 {
        self.addr + self.size
    }

    fn successors(&self) -> [Option<u32>; 2] {
        match self.end {
            End::Fall => [Some(self.next()), None],
            End::Jump(t) => [Some(t), None],
            End::Cond { target, .. } | End::Cbz { target, .. } => [Some(target), Some(self.next())],
        }
    }
}

fn low(v: u16) -> u8 {
    (v & 7) as u8
}

fn sext(v: u32, bits: u32) -> u32 {
    let s = 32 - bits;
    (((v << s) as i32) >> s) as u32
}

enum Insn {
    Op(Op),
    End(End),
}

/// Decodes the instruction `hw` at `pc`; `None`: not in the subset.
fn decode(hw: u16, pc: u32, m4: bool) -> Option<Insn> {
    let op = match hw >> 11 {
        0b00000..=0b00010 => {
            let kind = (hw >> 11) as u8;
            Op::ShiftImm { kind, rd: low(hw), rm: low(hw >> 3), imm: ((hw >> 6) & 0x1F) as u8 }
        }
        0b00011 => {
            let b = if hw & 0x0400 != 0 { Operand::Imm(u32::from((hw >> 6) & 7)) } else { Operand::Reg(low(hw >> 6)) };
            Op::AddSub { rd: low(hw), rn: low(hw >> 3), b, sub: hw & 0x0200 != 0 }
        }
        0b00100 => Op::MovImm { rd: low(hw >> 8), imm: u32::from(hw & 0xFF) },
        0b00101 => Op::CmpImm { rn: low(hw >> 8), imm: u32::from(hw & 0xFF) },
        0b00110 | 0b00111 => {
            let rdn = low(hw >> 8);
            Op::AddSub { rd: rdn, rn: rdn, b: Operand::Imm(u32::from(hw & 0xFF)), sub: hw >> 11 == 0b00111 }
        }
        0b01000 if hw & 0x0400 == 0 => Op::Alu { op: ((hw >> 6) & 0xF) as u8, rdn: low(hw), rm: low(hw >> 3) },
        0b01000 => {
            // high registers, BX/BLX
            let rd = (((hw >> 4) & 8) | (hw & 7)) as u8;
            let rm = ((hw >> 3) & 0xF) as u8;
            if rd == 15 || rm == 15 {
                return None;
            }
            match (hw >> 8) & 3 {
                0 => Op::AddHi { rd, rm },
                1 => Op::CmpHi { rn: rd, rm },
                2 => Op::MovHi { rd, rm },
                _ => return None,
            }
        }
        0b01001 => {
            let base = (pc + 4) & !3;
            Op::Load { rt: low(hw >> 8), addr: Addr::Abs(base + u32::from(hw & 0xFF) * 4), size: 4, signed: false }
        }
        0b01010 | 0b01011 => {
            let addr = Addr::Reg { rn: low(hw >> 3), rm: low(hw >> 6) };
            let (size, signed) = match (hw >> 9) & 7 {
                3 => (1, true),
                4 => (4, false),
                5 => (2, false),
                6 => (1, false),
                7 => (2, true),
                _ => return None, // stores
            };
            Op::Load { rt: low(hw), addr, size, signed }
        }
        0b01101 | 0b01111 | 0b10001 => {
            let size = match hw >> 11 {
                0b01101 => 4,
                0b01111 => 1,
                _ => 2,
            };
            let off = u32::from((hw >> 6) & 0x1F) * size;
            Op::Load { rt: low(hw), addr: Addr::Imm { rn: low(hw >> 3), off }, size: size as u8, signed: false }
        }
        0b10011 => {
            let off = u32::from(hw & 0xFF) * 4;
            Op::Load { rt: low(hw >> 8), addr: Addr::Imm { rn: 13, off }, size: 4, signed: false }
        }
        0b10100 => Op::Adr { rd: low(hw >> 8), value: ((pc + 4) & !3) + u32::from(hw & 0xFF) * 4 },
        0b10101 => Op::AddSp { rd: low(hw >> 8), imm: u32::from(hw & 0xFF) * 4 },
        0b10110 | 0b10111 => {
            if hw & 0xFF00 == 0xB000 {
                let imm = u32::from(hw & 0x7F) * 4;
                Op::AdjustSp { delta: if hw & 0x80 != 0 { imm.wrapping_neg() } else { imm } }
            } else if hw & 0xFF00 == 0xB200 {
                Op::Extend { kind: ((hw >> 6) & 3) as u8, rd: low(hw), rm: low(hw >> 3) }
            } else if m4 && hw & 0xF500 == 0xB100 {
                let off = (u32::from(hw >> 9) & 1) << 6 | u32::from((hw >> 3) & 0x1F) << 1;
                return Some(Insn::End(End::Cbz { rn: low(hw), nonzero: hw & 0x0800 != 0, target: pc + 4 + off }));
            } else if hw == 0xBF00 {
                Op::Nop
            } else {
                return None;
            }
        }
        0b11010 | 0b11011 => {
            let cond = ((hw >> 8) & 0xF) as u8;
            if cond >= 14 {
                return None; // UDF, SVC
            }
            let target = pc.wrapping_add(4).wrapping_add(sext(u32::from(hw & 0xFF) << 1, 9));
            return Some(Insn::End(End::Cond { cond, target }));
        }
        0b11100 => {
            let target = pc.wrapping_add(4).wrapping_add(sext(u32::from(hw & 0x7FF) << 1, 12));
            return Some(Insn::End(End::Jump(target)));
        }
        _ => return None,
    };
    Some(Insn::Op(op))
}

fn bit(r: u8) -> u32 {
    1 << r
}

/// Registers and flags `op` reads and writes (C of a shift by a register
/// is kept for a zero amount: read, not written).
fn op_regs(op: &Op) -> (u32, u32) {
    match *op {
        Op::ShiftImm { kind, rd, rm, imm } => {
            let c = if kind == 0 && imm == 0 { 0 } else { C };
            (bit(rm), bit(rd) | N | Z | c)
        }
        Op::AddSub { rd, rn, b, .. } => {
            let b = if let Operand::Reg(rm) = b { bit(rm) } else { 0 };
            (bit(rn) | b, bit(rd) | NZCV)
        }
        Op::MovImm { rd, .. } => (0, bit(rd) | N | Z),
        Op::CmpImm { rn, .. } => (bit(rn), NZCV),
        Op::Alu { op, rdn, rm } => match op {
            0 | 1 | 0xC | 0xE => (bit(rdn) | bit(rm), bit(rdn) | N | Z),
            2 | 3 | 4 | 7 => (bit(rdn) | bit(rm) | C, bit(rdn) | N | Z),
            5 | 6 => (bit(rdn) | bit(rm) | C, bit(rdn) | NZCV),
            8 => (bit(rdn) | bit(rm), N | Z),
            9 => (bit(rm), bit(rdn) | NZCV),
            0xA | 0xB => (bit(rdn) | bit(rm), NZCV),
            0xD => (bit(rdn) | bit(rm), bit(rdn) | N | Z),
            _ => (bit(rm), bit(rdn) | N | Z), // MVN
        },
        Op::AddHi { rd, rm } => (bit(rd) | bit(rm), bit(rd)),
        Op::CmpHi { rn, rm } => (bit(rn) | bit(rm), NZCV),
        Op::MovHi { rd, rm } | Op::Extend { rd, rm, .. } => (bit(rm), bit(rd)),
        Op::AddSp { rd, .. } => (bit(13), bit(rd)),
        Op::Adr { rd, .. } => (0, bit(rd)),
        Op::AdjustSp { .. } => (bit(13), bit(13)),
        Op::Load { rt, addr, .. } => {
            let uses = match addr {
                Addr::Imm { rn, .. } => bit(rn),
                Addr::Reg { rn, rm } => bit(rn) | bit(rm),
                Addr::Abs(_) => 0,
            };
            (uses, bit(rt))
        }
        Op::Nop => (0, 0),
    }
}

fn cond_flags(cond: u8) -> u32 {
    match cond >> 1 {
        0 => Z,
        1 => C,
        2 => N,
        3 => V,
        4 => C | Z,
        5 => N | V,
        _ => N | Z | V,
    }
}

/// Decodes the block the translator makes at `addr` (up to and including
/// its first branch, at most to the end of the page). `None`: something
/// outside the subset, or a load whose address the block computes itself
/// (addresses are checked before a block runs).
fn decode_block(addr: u32, fetch: &impl Fn(u32) -> Option<u16>, m4: bool) -> Option<Block> {
    let mut b = Block { addr, size: 0, ops: Vec::new(), end: End::Fall, uses: 0, defs: 0 };
    loop {
        let pc = addr + b.size;
        if b.size > 0 && pc.is_multiple_of(RAM_PAGE as u32) {
            return Some(b);
        }
        if b.size / 2 >= MAX_BLOCK_INSNS {
            return None;
        }
        let hw = fetch(pc)?;
        if hw >> 11 >= 0b11101 {
            return None; // 32-bit instruction
        }
        b.size += 2;
        match decode(hw, pc, m4)? {
            Insn::Op(op) => {
                let (uses, defs) = op_regs(&op);
                if matches!(op, Op::Load { .. }) && uses & b.defs != 0 {
                    return None;
                }
                b.uses |= uses & !b.defs;
                b.defs |= defs;
                b.ops.push(op);
            }
            Insn::End(end) => {
                let uses = match end {
                    End::Cond { cond, .. } => cond_flags(cond),
                    End::Cbz { rn, .. } => bit(rn),
                    _ => 0,
                };
                b.uses |= uses & !b.defs;
                b.end = end;
                return Some(b);
            }
        }
    }
}

/// The blocks reachable from `head` (head first), as far as they decode.
fn explore(head: u32, fetch: &impl Fn(u32) -> Option<u16>, m4: bool) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut todo = vec![head];
    let mut seen = vec![head];
    while let Some(a) = todo.pop() {
        if blocks.len() == MAX_BLOCKS {
            break;
        }
        let Some(b) = decode_block(a, fetch, m4) else { continue };
        for s in b.successors().into_iter().flatten() {
            if !seen.contains(&s) {
                seen.push(s);
                todo.push(s);
            }
        }
        blocks.push(b);
    }
    blocks.sort_by_key(|b| b.addr != head);
    blocks
}

/// Registers and flags live at the start of each block: read before they
/// are written, on some path. Leaving the blocks, everything is live.
fn live_in(blocks: &[Block]) -> Vec<u32> {
    let mut live = vec![0u32; blocks.len()];
    loop {
        let mut changed = false;
        for (i, b) in blocks.iter().enumerate() {
            let out = b
                .successors()
                .into_iter()
                .flatten()
                .fold(0, |acc, s| acc | blocks.iter().position(|x| x.addr == s).map_or(ALL, |j| live[j]));
            let l = b.uses | (out & !b.defs);
            if l != live[i] {
                live[i] = l;
                changed = true;
            }
        }
        if !changed {
            return live;
        }
    }
}

/// Core registers and flags while a loop runs here.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cpu {
    r: [u32; 16],
    n: bool,
    z: bool,
    c: bool,
    v: bool,
}

fn add_with_carry(x: u32, y: u32, carry: bool) -> (u32, bool, bool) {
    let (r1, c1) = x.overflowing_add(y);
    let (r, c2) = r1.overflowing_add(u32::from(carry));
    (r, c1 || c2, ((x ^ r) & (y ^ r)) >> 31 != 0)
}

/// Result and carry of shift `kind` (0 LSL, 1 LSR, 2 ASR, 3 ROR) by `n`
/// (register amounts: the bottom byte), `None` carry: unchanged.
fn shift(kind: u8, x: u32, n: u32) -> (u32, Option<bool>) {
    if n == 0 {
        return (x, None);
    }
    let bit = |i: u32| x >> i & 1 != 0;
    match kind {
        0 if n < 32 => (x << n, Some(bit(32 - n))),
        0 => (0, Some(n == 32 && bit(0))),
        1 if n < 32 => (x >> n, Some(bit(n - 1))),
        1 => (0, Some(n == 32 && bit(31))),
        2 if n < 32 => (((x as i32) >> n) as u32, Some(bit(n - 1))),
        2 => (((x as i32) >> 31) as u32, Some(bit(31))),
        _ => {
            let r = x.rotate_right(n & 31);
            (r, Some(r >> 31 != 0))
        }
    }
}

impl Cpu {
    fn nz(&mut self, r: u32) {
        self.n = r >> 31 != 0;
        self.z = r == 0;
    }

    fn nzcv(&mut self, (r, c, v): (u32, bool, bool)) -> u32 {
        self.nz(r);
        self.c = c;
        self.v = v;
        r
    }

    fn holds(&self, cond: u8) -> bool {
        let base = match cond >> 1 {
            0 => self.z,
            1 => self.c,
            2 => self.n,
            3 => self.v,
            4 => self.c && !self.z,
            5 => self.n == self.v,
            _ => !self.z && self.n == self.v,
        };
        base != (cond & 1 != 0)
    }

    fn addr(&self, a: Addr) -> u32 {
        match a {
            Addr::Imm { rn, off } => self.r[usize::from(rn)].wrapping_add(off),
            Addr::Reg { rn, rm } => self.r[usize::from(rn)].wrapping_add(self.r[usize::from(rm)]),
            Addr::Abs(a) => a,
        }
    }

    fn exec(&mut self, op: &Op, load: &mut impl FnMut(u32, u32) -> u32) {
        let r = |s: &Self, i: u8| s.r[usize::from(i)];
        match *op {
            Op::ShiftImm { kind, rd, rm, imm } => {
                // LSR/ASR #0 encode a shift by 32
                let n = if kind != 0 && imm == 0 { 32 } else { u32::from(imm) };
                let (v, c) = shift(kind, r(self, rm), n);
                self.r[usize::from(rd)] = v;
                self.nz(v);
                if let Some(c) = c {
                    self.c = c;
                }
            }
            Op::AddSub { rd, rn, b, sub } => {
                let b = match b {
                    Operand::Imm(i) => i,
                    Operand::Reg(rm) => r(self, rm),
                };
                let a = r(self, rn);
                let res = if sub { add_with_carry(a, !b, true) } else { add_with_carry(a, b, false) };
                self.r[usize::from(rd)] = self.nzcv(res);
            }
            Op::MovImm { rd, imm } => {
                self.r[usize::from(rd)] = imm;
                self.nz(imm);
            }
            Op::CmpImm { rn, imm } => {
                self.nzcv(add_with_carry(r(self, rn), !imm, true));
            }
            Op::Alu { op, rdn, rm } => {
                let (a, b) = (r(self, rdn), r(self, rm));
                let d = usize::from(rdn);
                match op {
                    0 | 1 | 0xC | 0xD | 0xE | 0xF => {
                        let v = match op {
                            0 => a & b,
                            1 => a ^ b,
                            0xC => a | b,
                            0xD => a.wrapping_mul(b),
                            0xE => a & !b,
                            _ => !b,
                        };
                        self.r[d] = v;
                        self.nz(v);
                    }
                    2 | 3 | 4 | 7 => {
                        let kind = match op {
                            2 => 0,
                            3 => 1,
                            4 => 2,
                            _ => 3,
                        };
                        let (v, c) = shift(kind, a, b & 0xFF);
                        self.r[d] = v;
                        self.nz(v);
                        if let Some(c) = c {
                            self.c = c;
                        }
                    }
                    5 => self.r[d] = self.nzcv(add_with_carry(a, b, self.c)),
                    6 => self.r[d] = self.nzcv(add_with_carry(a, !b, self.c)),
                    8 => self.nz(a & b),
                    9 => self.r[d] = self.nzcv(add_with_carry(0, !b, true)),
                    0xA => {
                        self.nzcv(add_with_carry(a, !b, true));
                    }
                    _ => {
                        self.nzcv(add_with_carry(a, b, false));
                    }
                }
            }
            Op::AddHi { rd, rm } => self.r[usize::from(rd)] = r(self, rd).wrapping_add(r(self, rm)),
            Op::CmpHi { rn, rm } => {
                self.nzcv(add_with_carry(r(self, rn), !r(self, rm), true));
            }
            Op::MovHi { rd, rm } => self.r[usize::from(rd)] = r(self, rm),
            Op::AddSp { rd, imm } => self.r[usize::from(rd)] = self.r[13].wrapping_add(imm),
            Op::Adr { rd, value } => self.r[usize::from(rd)] = value,
            Op::AdjustSp { delta } => self.r[13] = self.r[13].wrapping_add(delta),
            Op::Extend { kind, rd, rm } => {
                let x = r(self, rm);
                self.r[usize::from(rd)] = match kind {
                    0 => x as i16 as i32 as u32,
                    1 => x as i8 as i32 as u32,
                    2 => x & 0xFFFF,
                    _ => x & 0xFF,
                };
            }
            Op::Load { rt, addr, size, signed } => {
                let v = load(self.addr(addr), u32::from(size));
                self.r[usize::from(rt)] = match (signed, size) {
                    (true, 1) => v as u8 as i8 as i32 as u32,
                    (true, _) => v as u16 as i16 as i32 as u32,
                    _ => v,
                };
            }
            Op::Nop => {}
        }
    }

    /// The registers and flags in `mask`.
    fn masked(&self, mask: u32) -> Cpu {
        let mut m = *self;
        for (i, r) in m.r.iter_mut().enumerate() {
            if mask & (1 << i) == 0 {
                *r = 0;
            }
        }
        m.n &= mask & N != 0;
        m.z &= mask & Z != 0;
        m.c &= mask & C != 0;
        m.v &= mask & V != 0;
        m
    }
}

/// Where a load goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Memory,
    Periph,
}

/// The state at a visit of the loop head that decides the iterations after
/// it, and the head block's charge (entered the same way).
#[derive(Clone, Copy, PartialEq, Eq)]
struct HeadState {
    cpu: Cpu,
    cycles: u64,
    continues: bool,
}

impl System {
    fn load_target(&self, addr: u32, size: u32) -> Option<Target> {
        if !addr.is_multiple_of(size) {
            return None;
        }
        if self.config.memory.is_periph(addr) {
            Some(Target::Periph)
        } else {
            self.ram.read(addr, size).map(|_| Target::Memory)
        }
    }

    /// Runs the loop's blocks from the PC in `cpu` until it leaves them
    /// (`false`) or the slice ends or an interrupt is due as in the block
    /// hook (`true`, the hook would have stopped Unicorn there).
    fn run_poll(&mut self, blocks: &[Block], head_live: u32, cpu: &mut Cpu) -> bool {
        let head = blocks[0].addr;
        let mut last_head: Option<(HeadState, u64)> = None;
        loop {
            let pc = cpu.r[15];
            let Some(b) = blocks.iter().find(|b| b.addr == pc) else { return false };
            // A block runs here only if all its loads do (their addresses
            // depend on registers at its start only, see `decode_block`).
            let loads_ok = b.ops.iter().all(|op| match *op {
                Op::Load { addr, size, .. } => self.load_target(cpu.addr(addr), u32::from(size)).is_some(),
                _ => true,
            });
            if !loads_ok {
                return false;
            }
            let Some(slot) = self.block_insns.find(u64::from(pc), b.size) else { return false };

            // as the block hook
            let entered_by_branch = self.enter_block(u64::from(pc));
            if self.clock.cycles() >= self.slice_end && !self.continue_after_events() {
                return true;
            }
            let charge = self.block_charge(slot, u64::from(pc), b.size, entered_by_branch);
            if pc == head {
                let state =
                    HeadState { cpu: cpu.masked(head_live), cycles: charge.cycles, continues: charge.continues };
                if let Some((prev, at)) = last_head
                    && prev == state
                    && self.idle.clean
                {
                    // the same iteration again: skip those whose reads
                    // happen before the polled values change (as idle skipping)
                    let now = self.clock.cycles();
                    let until = self.idle.stable_until.min(self.slice_end);
                    let period = now - at;
                    if period > 0 && until > now {
                        let to = now + (until - now) / period * period;
                        if to >= self.slice_end {
                            if self.slice_end > now {
                                self.idle.skipped_cycles += self.slice_end - now;
                                self.clock.advance(self.slice_end - now);
                            }
                            if !self.continue_after_events() {
                                return true;
                            }
                        } else {
                            self.idle.skipped_cycles += to - now;
                            self.clock.advance(to - now);
                        }
                    }
                }
                last_head = Some((state, self.clock.cycles()));
                self.idle.restart_period();
            }
            self.commit_block(u64::from(pc), b.size, &charge);

            for op in &b.ops {
                cpu.exec(op, &mut |addr, size| {
                    if self.config.memory.is_periph(addr) {
                        self.mmio_read(addr, size)
                    } else {
                        self.ram.read(addr, size).unwrap_or(0)
                    }
                });
            }
            cpu.r[15] = match b.end {
                End::Fall => b.next(),
                End::Jump(t) => t,
                End::Cond { cond, target } => {
                    if cpu.holds(cond) {
                        target
                    } else {
                        b.next()
                    }
                }
                End::Cbz { rn, nonzero, target } => {
                    if (cpu.r[usize::from(rn)] != 0) == nonzero {
                        target
                    } else {
                        b.next()
                    }
                }
            };
        }
    }
}

/// IT bits of xPSR.
const XPSR_IT: u32 = 0x0600_FC00;

impl Machine {
    /// Runs the polling loop at the PC (armed by idle detection) in place of
    /// Unicorn. Returns true if Unicorn is to continue at the PC, false if
    /// the run ends where the block hook would have ended it.
    pub(super) fn fast_forward_poll(&mut self) -> bool {
        let head = self.pc();
        let debugging = !self.breakpoints.is_empty() || !self.hw_breakpoints.is_empty() || !self.watchpoints.is_empty();
        let mut all = [0u32; 17];
        // SAFETY: the engine of this machine, not running; `all` holds R0..R15 and xPSR.
        unsafe { uc_its_core_regs(self.uc.get_handle().cast(), all.as_mut_ptr()) };
        let m4 = self.uc.get_data().config.cpu == CpuModel::CortexM4;
        let blocks = if debugging || all[16] & XPSR_IT != 0 {
            Vec::new()
        } else {
            let uc = &self.uc;
            let fetch = |a: u32| {
                let mut b = [0u8; 2];
                uc.mem_read(u64::from(a), &mut b).ok().map(|()| u16::from_le_bytes(b))
            };
            explore(head, &fetch, m4)
        };
        let sys = self.uc.get_data_mut();
        if blocks.first().is_none_or(|b| b.addr != head) {
            sys.idle.poll_failed(u64::from(head));
            return true;
        }
        let live = live_in(&blocks)[0];
        let x = all[16];
        let mut cpu =
            Cpu { r: [0; 16], n: x >> 31 != 0, z: x >> 30 & 1 != 0, c: x >> 29 & 1 != 0, v: x >> 28 & 1 != 0 };
        cpu.r.copy_from_slice(&all[..16]);
        cpu.r[15] = head;
        let start = cpu;
        let t0 = sys.clock.cycles();
        let stopped = sys.run_poll(&blocks, live, &mut cpu);
        if !stopped && sys.clock.cycles() == t0 {
            // not even the head block runs here (a load elsewhere)
            sys.idle.poll_failed(u64::from(head));
        }
        // idle detection starts over: its observations are from before
        sys.idle.restart();
        if cpu != start {
            let nzcv =
                u32::from(cpu.n) << 31 | u32::from(cpu.z) << 30 | u32::from(cpu.c) << 29 | u32::from(cpu.v) << 28;
            // SAFETY: the engine of this machine, not running.
            unsafe { uc_its_set_core_regs(self.uc.get_handle().cast(), cpu.r.as_ptr(), nzcv) };
        }
        !stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(words: &[u16]) -> impl Fn(u32) -> Option<u16> + '_ {
        move |a: u32| words.get(((a - 0x1000) / 2) as usize).copied()
    }

    /// The Pico SDK's `busy_wait_until` (timer at r2, target in r3:r4).
    const BUSY_WAIT: [u16; 7] = [
        0x6a51, // 1000: ldr r1, [r2, #0x24]
        0x4299, // 1002: cmp r1, r3
        0xd102, // 1004: bne 100c
        0x6a91, // 1006: ldr r1, [r2, #0x28]
        0x42a1, // 1008: cmp r1, r4
        0xd3f9, // 100a: blo 1000
        0xbd70, // 100c: pop {r4, r5, r6, pc}
    ];

    #[test]
    fn decodes_the_loop_and_finds_the_polled_register_dead() {
        let blocks = explore(0x1000, &code(&BUSY_WAIT), false);
        assert_eq!(blocks.len(), 2, "the POP is not in the subset");
        assert_eq!((blocks[0].addr, blocks[0].size), (0x1000, 6));
        assert_eq!(blocks[0].end, End::Cond { cond: 1, target: 0x100C });
        assert_eq!(blocks[1].end, End::Cond { cond: 3, target: 0x1000 });
        let live = live_in(&blocks)[0];
        assert_eq!(live & bit(1), 0, "r1 is loaded before it is read");
        assert_eq!(live & NZCV, 0, "the flags are set before they are read");
        assert_ne!(live & bit(2), 0);
        assert_ne!(live & bit(4), 0);
    }

    #[test]
    fn a_load_from_a_computed_address_is_not_run_here() {
        // ldr r1, [r0]; ldr r2, [r1]; b .
        let words = [0x6801, 0x680a, 0xe7fc];
        assert!(decode_block(0x1000, &code(&words), false).is_none());
    }

    #[test]
    fn blocks_end_at_page_boundaries() {
        let b = decode_block(0x13FC, &|_| Some(0xbf00), false).unwrap();
        assert_eq!((b.size, b.end), (4, End::Fall));
    }

    #[test]
    fn flags_follow_the_architecture() {
        let mut cpu = Cpu { r: [0; 16], n: false, z: false, c: false, v: false };
        cpu.r[0] = 5;
        cpu.r[1] = 7;
        let mut none = |_, _| 0;
        // cmp r0, r1: 5 - 7 borrows
        cpu.exec(&Op::Alu { op: 0xA, rdn: 0, rm: 1 }, &mut none);
        assert!(cpu.n && !cpu.z && !cpu.c && !cpu.v);
        assert!(cpu.holds(3), "LO");
        // subs r0, #5 -> 0, no borrow
        cpu.exec(&Op::AddSub { rd: 0, rn: 0, b: Operand::Imm(5), sub: true }, &mut none);
        assert!(cpu.z && cpu.c && !cpu.n);
        // adds r2, r3 with 0x7FFFFFFF + 1 overflows
        cpu.r[2] = 0x7FFF_FFFF;
        cpu.r[3] = 1;
        cpu.exec(&Op::AddSub { rd: 2, rn: 2, b: Operand::Reg(3), sub: false }, &mut none);
        assert!(cpu.v && cpu.n && !cpu.c);
        // lsrs r1, r1, #1: carry out of bit 0
        cpu.r[1] = 3;
        cpu.exec(&Op::ShiftImm { kind: 1, rd: 1, rm: 1, imm: 1 }, &mut none);
        assert_eq!(cpu.r[1], 1);
        assert!(cpu.c);
    }
}
