//! Skipping counting delay loops (`for (i = n; i > 0; i--);`).
//!
//! Idle detection skips loops whose state repeats; a counting loop changes
//! its counter in every iteration. Here a small interpreter runs one
//! iteration of the loop at a time: from the head with the registers and the
//! memory of the head, back to the head. It knows only instructions whose
//! results are sums and differences of their inputs (add, sub, mov, loads and
//! stores at fixed addresses, compares, branches), so every value in an
//! iteration is affine in the iteration number. With exactly one conditional
//! branch in the loop, the first iteration that leaves it follows from its
//! operands, two lines in the iteration number: the core jumps to the head of
//! an iteration before it (and before the next event), with the registers,
//! the stored words and the cycles that running the iterations would give.
//!
//! The interpreter is checked against Unicorn first: at three visits of the
//! head in a row, the state Unicorn reached must be the one the interpreter
//! predicted, and the period in cycles the same. The jump interprets the last
//! skipped iteration again, from the extrapolated state, for the flags and
//! as a final check.

use std::collections::HashMap;

/// Registers R0..R14 (the PC is the loop head).
pub(super) type Regs = [u32; 15];

const MAX_INSNS: usize = 48;
const N: u32 = 1 << 31;
const Z: u32 = 1 << 30;
const C: u32 = 1 << 29;
const V: u32 = 1 << 28;
const NZCV: u32 = N | Z | C | V;

/// The flags the conditional branch tests, and how they were set.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Setter {
    /// a - b (CMP, SUBS, CBZ/CBNZ with b = 0)
    Sub(u32, u32),
    /// a + b (CMN, ADDS)
    Add(u32, u32),
    /// N and Z of a (MOVS)
    Mov(u32),
}

/// The loop's conditional branch in one iteration.
#[derive(Clone, Copy, Debug)]
struct Branch {
    pc: u32,
    cond: u8,
    setter: Setter,
    taken: bool,
}

/// One iteration from the head back to it.
#[derive(Debug)]
pub(super) struct Iteration {
    pub regs: Regs,
    /// N, Z, C, V (bits 31..28) at the head.
    pub nzcv: u32,
    /// Words stored, last value per address, in the order first stored.
    pub stores: Vec<(u32, u32)>,
    /// Addresses loaded (in order).
    loads: Vec<u32>,
    branch: Branch,
}

/// Memory as the interpreter sees it: RAM and flash.
pub(super) trait Memory {
    fn load(&self, addr: u32) -> Option<u32>;
    /// `addr` is RAM that a loop may store to (not code).
    fn storable(&self, addr: u32) -> bool;
}

fn sign_extend(v: u32, bits: u32) -> i32 {
    ((v << (32 - bits)) as i32) >> (32 - bits)
}

fn thumb_expand_imm(imm12: u32) -> u32 {
    let imm8 = imm12 & 0xFF;
    if imm12 >> 10 == 0 {
        match (imm12 >> 8) & 3 {
            0 => imm8,
            1 => imm8 | imm8 << 16,
            2 => imm8 << 8 | imm8 << 24,
            _ => imm8 * 0x0101_0101,
        }
    } else {
        (0x80 | (imm12 & 0x7F)).rotate_right(imm12 >> 7)
    }
}

/// Flags of `a + b + carry`.
fn add_flags(a: u32, b: u32, carry: bool) -> (u32, u32) {
    let wide = u64::from(a) + u64::from(b) + u64::from(carry);
    let r = wide as u32;
    let mut f = r & N;
    if r == 0 {
        f |= Z;
    }
    if wide > u64::from(u32::MAX) {
        f |= C;
    }
    if (!(a ^ b) & (a ^ r)) & N != 0 {
        f |= V;
    }
    (r, f)
}

fn setter_flags(s: Setter, old: u32) -> u32 {
    match s {
        Setter::Sub(a, b) => add_flags(a, !b, true).1,
        Setter::Add(a, b) => add_flags(a, b, false).1,
        Setter::Mov(a) => (old & (C | V)) | (a & N) | if a == 0 { Z } else { 0 },
    }
}

fn cond_holds(cond: u8, f: u32) -> bool {
    let (n, z, c, v) = (f & N != 0, f & Z != 0, f & C != 0, f & V != 0);
    match cond {
        0 => z,
        1 => !z,
        2 => c,
        3 => !c,
        4 => n,
        5 => !n,
        6 => v,
        7 => !v,
        8 => c && !z,
        9 => !c || z,
        10 => n == v,
        11 => n != v,
        12 => !z && n == v,
        13 => z || n != v,
        _ => true,
    }
}

struct Cpu<'a, M: Memory> {
    r: [u32; 16],
    nzcv: u32,
    /// Flags written in this iteration so far (a branch may only test these).
    written: u32,
    setter: Option<Setter>,
    mem: &'a M,
    /// Words of the head that differ from memory (extrapolated), then the stores.
    words: HashMap<u32, u32>,
    stores: Vec<(u32, u32)>,
    loads: Vec<u32>,
    branch: Option<Branch>,
}

impl<M: Memory> Cpu<'_, M> {
    fn load(&mut self, addr: u32) -> Option<u32> {
        if addr & 3 != 0 {
            return None;
        }
        self.loads.push(addr);
        match self.words.get(&addr) {
            Some(&v) => Some(v),
            None => self.mem.load(addr),
        }
    }

    fn store(&mut self, addr: u32, v: u32) -> Option<()> {
        if addr & 3 != 0 || !self.mem.storable(addr) {
            return None;
        }
        self.words.insert(addr, v);
        match self.stores.iter_mut().find(|(a, _)| *a == addr) {
            Some(s) => s.1 = v,
            None => self.stores.push((addr, v)),
        }
        Some(())
    }

    fn set_flags(&mut self, s: Setter) {
        let f = setter_flags(s, self.nzcv);
        let mask = if matches!(s, Setter::Mov(_)) { N | Z } else { NZCV };
        self.nzcv = (self.nzcv & !mask) | (f & mask);
        self.written |= mask;
        self.setter = Some(s);
    }

    /// A low register or R8..R12 (not SP, LR, PC as data).
    fn data_reg(r: u32) -> Option<usize> {
        (r < 13).then_some(r as usize)
    }

    fn add_sub(&mut self, rd: u32, rn: u32, op2: u32, sub: bool, s: bool) -> Option<()> {
        let a = self.r[rn as usize];
        if s {
            self.set_flags(if sub { Setter::Sub(a, op2) } else { Setter::Add(a, op2) });
        }
        if rd != 15 {
            // SP or LR as a destination would leave the loop's stack frame
            let rd = Self::data_reg(rd)?;
            self.r[rd] = if sub { a.wrapping_sub(op2) } else { a.wrapping_add(op2) };
        }
        Some(())
    }

    fn cond_branch(&mut self, pc: u32, cond: u8, target: u32, next: u32) -> Option<u32> {
        // the flags must come from this iteration, set by an instruction known here
        let needs = match cond {
            0 | 1 => Z,
            2 | 3 => C,
            4 | 5 => N,
            8 | 9 => C | Z,
            10 | 11 => N | V,
            12 | 13 => N | Z | V,
            _ => return None,
        };
        let setter = self.setter?;
        if self.written & needs != needs || self.branch.is_some() {
            return None;
        }
        let ordered = !matches!(cond, 0 | 1);
        if ordered && !matches!(setter, Setter::Sub(..)) {
            return None;
        }
        let taken = cond_holds(cond, self.nzcv);
        self.branch = Some(Branch { pc, cond, setter, taken });
        Some(if taken { target } else { next })
    }

    /// Executes the instruction at `pc`; returns the next PC.
    fn step(&mut self, pc: u32) -> Option<u32> {
        let h = self.mem.load(pc & !3).map(|w| if pc & 2 != 0 { w >> 16 } else { w & 0xFFFF })?;
        if h & 0xF800 >= 0xE800 {
            let h2 = self.mem.load((pc + 2) & !3).map(|w| if (pc + 2) & 2 != 0 { w >> 16 } else { w & 0xFFFF })?;
            return self.step32(pc, h, h2);
        }
        let next = pc + 2;
        let lo = |s: u32| (h >> s) & 7;
        match h {
            0xBF00 => {}
            _ if h & 0xFFC0 == 0x0000 => {
                // MOVS Rd, Rm (LSLS #0)
                let v = self.r[lo(3) as usize];
                self.r[lo(0) as usize] = v;
                self.set_flags(Setter::Mov(v));
            }
            _ if h & 0xF800 == 0x2000 => {
                let v = h & 0xFF;
                self.r[lo(8) as usize] = v;
                self.set_flags(Setter::Mov(v));
            }
            _ if h & 0xFC00 == 0x1C00 => self.add_sub(lo(0), lo(3), lo(6), h & 0x200 != 0, true)?,
            _ if h & 0xFC00 == 0x1800 => {
                let m = self.r[lo(6) as usize];
                self.add_sub(lo(0), lo(3), m, h & 0x200 != 0, true)?;
            }
            _ if h & 0xF000 == 0x3000 => self.add_sub(lo(8), lo(8), h & 0xFF, h & 0x800 != 0, true)?,
            _ if h & 0xF800 == 0x2800 => self.add_sub(15, lo(8), h & 0xFF, true, true)?,
            _ if h & 0xFFC0 == 0x4280 => {
                let m = self.r[lo(3) as usize];
                self.add_sub(15, lo(0), m, true, true)?;
            }
            _ if h & 0xFFC0 == 0x42C0 => {
                let m = self.r[lo(3) as usize];
                self.add_sub(15, lo(0), m, false, true)?;
            }
            _ if h & 0xFF00 == 0x4500 => {
                let (n, m) = ((h & 7) | ((h >> 4) & 8), (h >> 3) & 0xF);
                if n == 15 || m == 15 {
                    return None;
                }
                let mv = self.r[m as usize];
                self.add_sub(15, n, mv, true, true)?;
            }
            _ if h & 0xFF00 == 0x4400 => {
                // ADD Rdn, Rm (no flags)
                let (d, m) = ((h & 7) | ((h >> 4) & 8), (h >> 3) & 0xF);
                let d = Self::data_reg(d)?;
                let mv = self.r[Self::data_reg(m)?];
                self.r[d] = self.r[d].wrapping_add(mv);
            }
            _ if h & 0xFF00 == 0x4600 => {
                let (d, m) = ((h & 7) | ((h >> 4) & 8), (h >> 3) & 0xF);
                let d = Self::data_reg(d)?;
                self.r[d] = self.r[Self::data_reg(m)?];
            }
            _ if h & 0xF000 == 0x6000 => {
                let addr = self.r[lo(3) as usize].wrapping_add(((h >> 6) & 0x1F) * 4);
                self.load_store(lo(0), addr, h & 0x800 != 0)?;
            }
            _ if h & 0xF000 == 0x9000 => {
                let addr = self.r[13].wrapping_add((h & 0xFF) * 4);
                self.load_store(lo(8), addr, h & 0x800 != 0)?;
            }
            _ if h & 0xF000 == 0x5000 => {
                // LDR/STR Rt, [Rn, Rm] (word forms only)
                let addr = self.r[lo(3) as usize].wrapping_add(self.r[lo(6) as usize]);
                match h & 0xFE00 {
                    0x5800 => self.load_store(lo(0), addr, true)?,
                    0x5000 => self.load_store(lo(0), addr, false)?,
                    _ => return None,
                }
            }
            _ if h & 0xF800 == 0x4800 => {
                let addr = ((pc + 4) & !3) + (h & 0xFF) * 4;
                let v = self.load(addr)?;
                self.r[lo(8) as usize] = v;
            }
            _ if h & 0xF800 == 0xA800 => self.r[lo(8) as usize] = self.r[13].wrapping_add((h & 0xFF) * 4),
            _ if h & 0xF500 == 0xB100 => {
                let rn = lo(0);
                let off = ((h >> 3) & 0x1F) << 1 | ((h >> 9) & 1) << 6;
                // CBZ: taken if Rn == 0 (EQ of Rn - 0), CBNZ: NE
                let a = self.r[rn as usize];
                let (saved, setter, written) = (self.nzcv, self.setter, self.written);
                self.set_flags(Setter::Sub(a, 0));
                let to = self.cond_branch(pc, if h & 0x800 != 0 { 1 } else { 0 }, pc + 4 + off, next);
                // CBZ/CBNZ leave the flags alone
                (self.nzcv, self.setter, self.written) = (saved, setter, written);
                return to;
            }
            _ if h & 0xF000 == 0xD000 => {
                let cond = ((h >> 8) & 0xF) as u8;
                let target = (pc as i32 + 4 + sign_extend(h & 0xFF, 8) * 2) as u32;
                return self.cond_branch(pc, cond, target, next);
            }
            _ if h & 0xF800 == 0xE000 => return Some((pc as i32 + 4 + sign_extend(h & 0x7FF, 11) * 2) as u32),
            _ => return None,
        }
        Some(next)
    }

    fn load_store(&mut self, rt: u32, addr: u32, load: bool) -> Option<()> {
        let rt = Self::data_reg(rt)?;
        if load {
            self.r[rt] = self.load(addr)?;
            Some(())
        } else {
            self.store(addr, self.r[rt])
        }
    }

    fn step32(&mut self, pc: u32, h1: u32, h2: u32) -> Option<u32> {
        let next = pc + 4;
        let (rn, rd) = (h1 & 0xF, (h2 >> 8) & 0xF);
        let imm12 = ((h1 >> 10) & 1) << 11 | ((h2 >> 12) & 7) << 8 | (h2 & 0xFF);
        match (h1, h2) {
            (0xF3AF, 0x8000) => {}
            _ if h1 & 0xFFE0 == 0xF8C0 => {
                // LDR.W / STR.W Rt, [Rn, #imm12]
                if rn == 15 {
                    return None;
                }
                let addr = self.r[rn as usize].wrapping_add(h2 & 0xFFF);
                self.load_store(h2 >> 12, addr, h1 & 0x10 != 0)?;
            }
            _ if h1 & 0xFFE0 == 0xF840 && h1 & 0xF != 15 => {
                // LDR/STR Rt, [Rn, #+/-imm8] (offset form, no writeback)
                let puw = (h2 >> 8) & 0xF;
                let addr = match puw {
                    0b1100 => self.r[rn as usize].wrapping_sub(h2 & 0xFF),
                    0b1110 => self.r[rn as usize].wrapping_add(h2 & 0xFF),
                    _ => return None,
                };
                self.load_store(h2 >> 12, addr, h1 & 0x10 != 0)?;
            }
            _ if h1 & 0xFBE0 == 0xF100 || h1 & 0xFBE0 == 0xF1A0 => {
                // ADD/SUB(S).W Rd, Rn, #const (CMN/CMP with Rd = PC)
                if h2 & 0x8000 != 0 || rn == 15 || (rd == 15 && h1 & 0x10 == 0) {
                    return None;
                }
                self.add_sub(rd, rn, thumb_expand_imm(imm12), h1 & 0xFBE0 == 0xF1A0, h1 & 0x10 != 0)?;
            }
            _ if h1 & 0xFBF0 == 0xF200 || h1 & 0xFBF0 == 0xF2A0 => {
                // ADDW/SUBW Rd, Rn, #imm12
                if h2 & 0x8000 != 0 || rn == 15 {
                    return None;
                }
                self.add_sub(rd, rn, imm12, h1 & 0xFBF0 == 0xF2A0, false)?;
                if rd == 15 {
                    return None;
                }
            }
            _ if h1 & 0xFFE0 == 0xEB00 || h1 & 0xFFE0 == 0xEBA0 => {
                // ADD/SUB(S).W Rd, Rn, Rm without shift (CMN/CMP with Rd = PC)
                let rm = h2 & 0xF;
                if h2 & 0x70F0 != 0 || rn == 15 || rm >= 13 || (rd == 15 && h1 & 0x10 == 0) {
                    return None;
                }
                let m = self.r[rm as usize];
                self.add_sub(rd, rn, m, h1 & 0xFFE0 == 0xEBA0, h1 & 0x10 != 0)?;
            }
            _ if h1 & 0xFBF0 == 0xF240 && h2 & 0x8000 == 0 => {
                // MOVW
                let d = Self::data_reg(rd)?;
                self.r[d] = (h1 & 0xF) << 12 | imm12;
            }
            _ if h1 & 0xFBFF == 0xF04F && h2 & 0x8000 == 0 => {
                // MOV.W Rd, #const (without flags)
                let d = Self::data_reg(rd)?;
                self.r[d] = thumb_expand_imm(imm12);
            }
            _ if h1 & 0xF800 == 0xF000 && h2 & 0xD000 == 0x8000 => {
                let cond = ((h1 >> 6) & 0xF) as u8;
                let off = (h1 & 0x3F) << 12 | (h2 & 0x7FF) << 1 | ((h2 >> 13) & 1) << 18 | ((h2 >> 11) & 1) << 19;
                let off = sign_extend(off | ((h1 >> 10) & 1) << 20, 21);
                return self.cond_branch(pc, cond, (pc as i32 + 4 + off) as u32, next);
            }
            _ if h1 & 0xF800 == 0xF000 && h2 & 0xD000 == 0x9000 => {
                let s = (h1 >> 10) & 1;
                let i1 = !((h2 >> 13) & 1 ^ s) & 1;
                let i2 = !((h2 >> 11) & 1 ^ s) & 1;
                let off = s << 24 | i1 << 23 | i2 << 22 | (h1 & 0x3FF) << 12 | (h2 & 0x7FF) << 1;
                return Some((pc as i32 + 4 + sign_extend(off, 25)) as u32);
            }
            _ => return None,
        }
        Some(next)
    }
}

/// Runs one iteration of the loop at `head` from `regs` (the flags `nzcv`)
/// and the memory, where `words` overrides words of it. `None` unless it
/// returns to the head with exactly one conditional branch on the way.
pub(super) fn iterate<M: Memory>(
    mem: &M,
    head: u32,
    regs: &Regs,
    nzcv: u32,
    words: &[(u32, u32)],
) -> Option<Iteration> {
    let mut r = [0; 16];
    r[..15].copy_from_slice(regs);
    let mut cpu = Cpu {
        r,
        nzcv: nzcv & NZCV,
        written: 0,
        setter: None,
        mem,
        words: words.iter().copied().collect(),
        stores: Vec::new(),
        loads: Vec::new(),
        branch: None,
    };
    let mut pc = head;
    for _ in 0..MAX_INSNS {
        cpu.r[15] = pc + 4;
        pc = cpu.step(pc)?;
        if pc == head {
            let mut regs = [0; 15];
            regs.copy_from_slice(&cpu.r[..15]);
            return Some(Iteration { regs, nzcv: cpu.nzcv, stores: cpu.stores, loads: cpu.loads, branch: cpu.branch? });
        }
    }
    None
}

/// The flags of `setter` at iteration `k`, its operands lines from iteration
/// 0 and 1.
fn setter_at(s0: Setter, s1: Setter, k: u64) -> Option<Setter> {
    let line = |a0: u32, a1: u32| a0.wrapping_add((k as u32).wrapping_mul(a1.wrapping_sub(a0)));
    Some(match (s0, s1) {
        (Setter::Sub(a0, b0), Setter::Sub(a1, b1)) => Setter::Sub(line(a0, a1), line(b0, b1)),
        (Setter::Add(a0, b0), Setter::Add(a1, b1)) => Setter::Add(line(a0, a1), line(b0, b1)),
        (Setter::Mov(a0), Setter::Mov(a1)) => Setter::Mov(line(a0, a1)),
        _ => return None,
    })
}

/// Iterations from 0 on during which a value starting at `v` and changing by
/// `step` (signed) stays in its half of the 32-bit range (no wrap of either
/// the signed or the unsigned reading): the last such iteration.
fn steps_in_half(v: u32, step: u32) -> u64 {
    let s = i64::from(step as i32);
    let v = i64::from(v);
    let half = 1i64 << 31;
    if s == 0 {
        return u64::MAX;
    }
    let base = v / half * half;
    let n = if s > 0 { (base + half - 1 - v) / s } else { (v - base) / -s };
    n as u64
}

/// Smallest k >= 0 with `f0 + k * df == 0` (mod 2^32).
fn solve_zero(f0: u32, df: u32) -> Option<u64> {
    if f0 == 0 {
        return Some(0);
    }
    if df == 0 {
        return None;
    }
    let tz = df.trailing_zeros();
    let need = f0.wrapping_neg();
    if need.trailing_zeros() < tz {
        return None;
    }
    let modulus = 1u64 << (32 - tz);
    let odd = u64::from(df >> tz);
    // inverse of an odd number modulo 2^n (Newton)
    let mut inv = odd;
    for _ in 0..5 {
        inv = inv.wrapping_mul(2u64.wrapping_sub(odd.wrapping_mul(inv)));
    }
    Some((u64::from(need >> tz).wrapping_mul(inv)) & (modulus - 1))
}

/// The first iteration (from 0) that leaves the loop, among the first
/// `limit`, if the branch's operands are the lines through iterations 0
/// and 1; `limit` when none of them does (or it cannot tell beyond).
fn first_exit(b0: &Branch, b1: &Branch, limit: u64) -> Option<u64> {
    let exits = |k: u64| -> Option<bool> {
        let s = setter_at(b0.setter, b1.setter, k)?;
        Some(cond_holds(b0.cond, setter_flags(s, 0)) != b0.taken)
    };
    if matches!(b0.cond, 0 | 1) {
        // EQ/NE: the result is zero or not; it is zero at one point per lap
        let value = |s: Setter| match s {
            Setter::Sub(a, b) => a.wrapping_sub(b),
            Setter::Add(a, b) => a.wrapping_add(b),
            Setter::Mov(a) => a,
        };
        let (f0, f1) = (value(b0.setter), value(b1.setter));
        let df = f1.wrapping_sub(f0);
        // the loop leaves when the result becomes zero, or when it stops being zero
        let k = if exits(0)? {
            0
        } else if (b0.cond == 0) == b0.taken {
            // continues while zero: leaves at the first non-zero result
            if df == 0 { limit } else { 1 }
        } else {
            solve_zero(f0, df).unwrap_or(limit)
        };
        return Some(k.min(limit));
    }
    let (a0, b0v, a1, b1v) = match (b0.setter, b1.setter) {
        (Setter::Sub(a0, b0), Setter::Sub(a1, b1)) => (a0, b0, a1, b1),
        _ => return None,
    };
    // ordered compares are integer compares of the operands: monotone while
    // neither operand wraps
    let safe = steps_in_half(a0, a1.wrapping_sub(a0)).min(steps_in_half(b0v, b1v.wrapping_sub(b0v)));
    let last = safe.min(limit.saturating_sub(1));
    if !exits(last)? {
        return Some((last + 1).min(limit));
    }
    let (mut lo, mut hi) = (0u64, last); // exits(hi)
    if exits(0)? {
        return Some(0);
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if exits(mid)? { hi = mid } else { lo = mid }
    }
    Some(hi)
}

/// The analysis of a loop from its head state: how many iterations may be
/// skipped and the state after them.
pub(super) struct Jump {
    pub iterations: u64,
    pub regs: Regs,
    pub nzcv: u32,
    pub words: Vec<(u32, u32)>,
}

fn line(v0: u32, v1: u32, k: u64) -> u32 {
    v0.wrapping_add((k as u32).wrapping_mul(v1.wrapping_sub(v0)))
}

/// Why there is no jump.
#[derive(Debug, PartialEq)]
pub(super) enum NoJump {
    /// The loop is not one the jump can skip (not linear, several paths).
    Never,
    /// It leaves the loop or reaches `limit` within a few iterations.
    NotNow,
}

/// From the head state (`regs`, `nzcv`), plans the jump over the loop's
/// iterations before the first one that leaves it, at most `limit`.
pub(super) fn plan<M: Memory>(mem: &M, head: u32, regs: &Regs, nzcv: u32, limit: u64) -> Result<Jump, NoJump> {
    plan_checked(mem, head, regs, nzcv, limit)?.ok_or(NoJump::NotNow)
}

fn plan_checked<M: Memory>(mem: &M, head: u32, regs: &Regs, nzcv: u32, limit: u64) -> Result<Option<Jump>, NoJump> {
    let never = NoJump::Never;
    let i0 = iterate(mem, head, regs, nzcv, &[]).ok_or(NoJump::Never)?;
    // a loop that leaves within two iterations is not linear yet
    let Some(i1) = iterate(mem, head, &i0.regs, i0.nzcv, &i0.stores) else { return Ok(None) };
    let Some(i2) = iterate(mem, head, &i1.regs, i1.nzcv, &i1.stores) else { return Ok(None) };
    // the same path through the loop, the same memory words
    let same_path = |a: &Iteration, b: &Iteration| {
        a.branch.pc == b.branch.pc
            && a.branch.taken == b.branch.taken
            && a.loads == b.loads
            && a.stores.len() == b.stores.len()
            && a.stores.iter().zip(&b.stores).all(|(x, y)| x.0 == y.0)
    };
    if !same_path(&i0, &i1) || !same_path(&i1, &i2) {
        return Err(never);
    }
    // linear: the same change per iteration
    let words0: Vec<(u32, u32)> =
        i0.stores.iter().map(|&(a, _)| Some((a, mem.load(a)?))).collect::<Option<_>>().ok_or(NoJump::Never)?;
    let step = |a: &Regs, b: &Regs| -> Vec<u32> { a.iter().zip(b).map(|(x, y)| y.wrapping_sub(*x)).collect() };
    let d = step(regs, &i0.regs);
    if step(&i0.regs, &i1.regs) != d || step(&i1.regs, &i2.regs) != d {
        return Err(never);
    }
    for ((w0, s0), (s1, s2)) in words0.iter().zip(&i0.stores).zip(i1.stores.iter().zip(&i2.stores)) {
        let d = s0.1.wrapping_sub(w0.1);
        if s1.1.wrapping_sub(s0.1) != d || s2.1.wrapping_sub(s1.1) != d {
            return Err(never);
        }
    }
    let m = first_exit(&i0.branch, &i1.branch, limit).ok_or(NoJump::Never)?;
    if m < 3 {
        return Ok(None);
    }
    // the head of iteration m - 1, extrapolated, and that iteration run for
    // the flags at the head of iteration m (and as a check)
    let regs_m1: Regs = std::array::from_fn(|j| line(regs[j], i0.regs[j], m - 1));
    let words_m1: Vec<(u32, u32)> =
        words0.iter().zip(&i0.stores).map(|(&(a, w), &(_, s))| (a, line(w, s, m - 1))).collect();
    // (a mismatch here would be an error in the reasoning above: never again)
    let last = iterate(mem, head, &regs_m1, i0.nzcv, &words_m1).ok_or(NoJump::Never)?;
    if !same_path(&i0, &last) {
        return Err(never);
    }
    if last.regs.iter().zip(regs.iter().zip(&i0.regs)).any(|(&l, (&r0, &r1))| l != line(r0, r1, m)) {
        return Err(never);
    }
    for ((&(_, w), &(_, s)), &(_, l)) in words0.iter().zip(&i0.stores).zip(&last.stores) {
        if l != line(w, s, m) {
            return Err(never);
        }
    }
    Ok(Some(Jump { iterations: m, regs: last.regs, nzcv: last.nzcv, words: last.stores }))
}

/// Follows one candidate loop head over consecutive visits (see the module
/// documentation).
#[derive(Default)]
pub(super) struct DelayDetector {
    head: u64,
    /// The last visit: its cycle and the interpreter's prediction for the next.
    last: Option<(u64, Option<Iteration>)>,
    period: Option<u64>,
    /// Nothing but registers and RAM changed since the last visit.
    pub clean: bool,
    /// Heads the interpreter cannot run (most recent first).
    rejected: [u64; 8],
    pub skipped_cycles: u64,
}

impl DelayDetector {
    pub fn forget(&mut self) {
        self.last = None;
        self.period = None;
    }

    /// At a visit of the head `head` at cycle `now` with the head state:
    /// `Some(prediction checked)` when the loop ran as the interpreter
    /// predicted twice with the same period (then `period()` is valid).
    pub fn visit<M: Memory>(&mut self, mem: &M, head: u64, now: u64, regs: &Regs, nzcv: u32) -> bool {
        if head != self.head {
            if self.rejected.contains(&head) {
                return false;
            }
            self.head = head;
            self.forget();
        } else if self.rejected.contains(&head) {
            return false;
        }
        let clean = std::mem::replace(&mut self.clean, true);
        // Unicorn ran the last iteration as predicted, in `p` cycles
        let ran = match &self.last {
            Some((at, Some(pred))) if clean => (pred.regs == *regs
                && pred.nzcv == nzcv & NZCV
                && pred.stores.iter().all(|&(a, v)| mem.load(a) == Some(v)))
            .then_some(now - at),
            _ => None,
        };
        let matched = ran.is_some() && self.period == ran;
        self.period = ran;
        let pred = iterate(mem, head as u32, regs, nzcv, &[]);
        if pred.is_none() {
            self.rejected.rotate_right(1);
            self.rejected[0] = head;
            self.forget();
            return false;
        }
        self.last = Some((now, pred));
        matched
    }

    /// The jump can never skip the loop at the current head.
    pub fn reject(&mut self) {
        self.rejected.rotate_right(1);
        self.rejected[0] = self.head;
        self.forget();
    }

    pub fn period(&self) -> u64 {
        self.period.unwrap_or(0)
    }

    /// The interpreter runs the loop at `head` (it has not given up on it).
    pub fn following(&self, head: u64) -> bool {
        self.head == head && !self.rejected.contains(&head) && matches!(self.last, Some((_, Some(_))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thumb code at 0x0800_0000 and words of RAM at 0x2000_0000.
    struct Mem {
        code: Vec<u16>,
        ram: HashMap<u32, u32>,
    }

    impl Memory for Mem {
        fn load(&self, addr: u32) -> Option<u32> {
            if (0x0800_0000..0x0800_1000).contains(&addr) {
                let i = ((addr - 0x0800_0000) / 2) as usize;
                let h = |i: usize| u32::from(self.code.get(i).copied().unwrap_or(0));
                return Some(h(i) | h(i + 1) << 16);
            }
            self.ram.get(&addr).copied().or(((0x2000_0000..0x2000_1000).contains(&addr)).then_some(0))
        }
        fn storable(&self, addr: u32) -> bool {
            (0x2000_0000..0x2000_1000).contains(&addr)
        }
    }

    #[test]
    fn driver_delay_us_at_o0() {
        // head: ldr r0,[sp]; cbz r0, out; b; b; ldr r0,[sp]; subs r0,#1; str r0,[sp]; b head
        let code = vec![0x9800, 0xB128, 0xE7FF, 0xE7FF, 0x9800, 0x3801, 0x9000, 0xE7F7, 0xB002, 0x4770];
        let sp = 0x2000_0100;
        let mem = Mem { code, ram: HashMap::from([(sp, 1000)]) };
        let mut regs = [0; 15];
        regs[0] = 1000;
        regs[13] = sp;
        let j = plan(&mem, 0x0800_0000, &regs, C, u64::MAX).expect("plan");
        // iterations 0..999 decrement, iteration 1000 finds 0 and leaves
        assert_eq!(j.iterations, 1000);
        assert_eq!(j.regs[0], 0);
        assert_eq!(j.words, vec![(sp, 0)]);
        assert_eq!(j.nzcv & (Z | C), Z | C, "SUBS gave 0");
        let j = plan(&mem, 0x0800_0000, &regs, C, 300).expect("plan");
        assert_eq!((j.iterations, j.regs[0], j.nzcv & Z), (300, 700, 0));
    }

    #[test]
    fn counting_up_to_a_register_at_o2() {
        // head: adds r0, #1; cmp r0, r1; blt head
        let code = vec![0x3001, 0x4288, 0xDBFC];
        let mem = Mem { code, ram: HashMap::new() };
        let mut regs = [0; 15];
        regs[1] = 500;
        let j = plan(&mem, 0x0800_0000, &regs, 0, u64::MAX).expect("plan");
        // iteration k ends with r0 = k + 1; iteration 499 reaches 500 and leaves
        assert_eq!(j.iterations, 499);
        assert_eq!(j.regs[0], 499);
    }

    #[test]
    fn a_loop_that_doubles_is_not_skipped() {
        // head: adds r0, r0, r0; cmp r0, r1; blo head
        let code = vec![0x1800, 0x4288, 0xD3FC];
        let mem = Mem { code, ram: HashMap::new() };
        let mut regs = [0; 15];
        regs[0] = 1;
        regs[1] = 1 << 30;
        assert_eq!(plan(&mem, 0x0800_0000, &regs, 0, u64::MAX).err(), Some(NoJump::Never));
    }

    #[test]
    fn exit_at_a_wrap_of_the_counter() {
        // head: subs r0, #1; cmp r0, #10; bne head   (starts below 10: wraps)
        let code = vec![0x3801, 0x280A, 0xD1FC];
        let mem = Mem { code, ram: HashMap::new() };
        let mut regs = [0; 15];
        regs[0] = 5;
        let j = plan(&mem, 0x0800_0000, &regs, 0, u64::MAX).expect("plan");
        // after iteration k, r0 = 4 - k: equals 10 (leaves) at k = 2^32 - 6
        assert_eq!(j.iterations, (1u64 << 32) - 6);
        assert_eq!(j.regs[0], 11);
    }

    #[test]
    fn solve_zero_finds_the_first_root() {
        assert_eq!(solve_zero(1000, 0xFFFF_FFFF), Some(1000));
        assert_eq!(solve_zero(6, (-2i32) as u32), Some(3));
        assert_eq!(solve_zero(5, 2), None);
        assert_eq!(solve_zero(4, 2), Some((1 << 31) - 2));
    }
}
