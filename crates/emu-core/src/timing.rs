//! Cortex-M4 instruction timing.
//!
//! Translated blocks get a static cycle count when they are first executed.
//! What depends on run-time values is added by the machine: one pipeline
//! refill cycle when a block ends with a taken conditional branch, the operand-dependent
//! duration of divisions, bus wait states of peripheral accesses, and
//! exception entry/return.
//!
//! The costs were measured on an STM32F429 at 180 MHz (flash with ART
//! accelerator, code and data in cache) with DWT->CYCCNT, see the `cyc` and
//! `div` commands of `tests/fixtures/ac6-src/HwCheck`. They agree with the
//! Cortex-M4 TRM where it gives fixed values:
//!
//! | instruction | cycles |
//! |---|---|
//! | data processing, MUL/MLA/UMULL, IT, STR (any width) | 1 |
//! | LDR (any width) | 2, 1 if the previous instruction was a load whose result it does not use as address |
//! | LDM/STM/PUSH/POP, LDRD/STRD, VLDM/VSTM | 1 + registers |
//! | B, BL, BX, POP/LDR to PC | 1 + 1 (pipeline refill, also to the next instruction) |
//! | B<cond>, CBZ/CBNZ | 1, + 1 if taken |
//! | BLX register | 3 |
//! | TBB/TBH | 3 |
//! | UDIV/SDIV | 2..12 by operand size, see [`div_cycles`] |
//! | VADD/VSUB/VMUL and other VFP data processing | 1 |
//! | VMLA/VMLS/VFMA ... | 3 |
//! | VDIV, VSQRT | 14 |

/// Static properties of a translated block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockTiming {
    pub insns: u32,
    /// Cycles if the final conditional branch (if any) is not taken.
    pub cycles: u32,
    /// The block contains stores (STR, PUSH, STM, VSTR, ...).
    pub stores: bool,
    /// The block may change state that idle detection does not compare
    /// (MSR, CPS, SVC, floating-point registers).
    pub side_effects: bool,
    /// The block ends with a conditional branch: one more cycle if it is taken.
    pub ends_with_branch: bool,
    /// The block ends with an unconditional branch (refill already counted).
    pub ends_with_jump: bool,
    /// Extra refill cycle when the block is entered by a branch: its first
    /// instruction is 32 bits wide and not word-aligned (measured: a branch
    /// to an unaligned 32-bit instruction costs 3 instead of 2 cycles).
    pub entry_penalty: u32,
    /// Base register of the first instruction if it is a single load, and
    /// destination of the last one if that is a single load: a load at the
    /// start of a block that continues the previous one sequentially (the
    /// translator splits blocks e.g. at page boundaries) pipelines with it.
    pub first_load_rn: Option<u8>,
    pub last_load_rt: Option<u8>,
    /// Sources of the first instruction and destination of the last one if
    /// they are VFP data processing: a dependent chain continues across a
    /// block split (blocks end at 1 KiB boundaries) with its stall.
    pub first_fp_src: Option<[u8; 3]>,
    pub last_fp_dst: Option<u8>,
}

impl BlockTiming {
    /// Cycles saved when this block directly continues one whose last
    /// instruction was a load into `prev_rt`.
    pub fn continuation_saving(&self, prev_rt: Option<u8>) -> u32 {
        match (prev_rt, self.first_load_rn) {
            (Some(rt), Some(rn)) if rt != rn => 1,
            _ => 0,
        }
    }

    /// Stall when this block directly continues one whose last instruction
    /// was a VFP operation writing `prev_dst`.
    pub fn continuation_stall(&self, prev_dst: Option<u8>) -> u32 {
        match (prev_dst, self.first_fp_src) {
            (Some(d), Some(src)) if src.contains(&d) => 1,
            _ => 0,
        }
    }
}

/// A division instruction whose duration depends on its operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DivSite {
    pub addr: u32,
    pub signed: bool,
    /// Dividend register.
    pub rn: u8,
    /// Divisor register.
    pub rm: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Other,
    /// Single load; `rn` = base register, `rt` = destination.
    Load {
        rn: u8,
        rt: u8,
    },
    Store,
    /// LDM/STM/PUSH/POP and friends (not pipelined with neighbours).
    Multiple,
    Div {
        signed: bool,
        rn: u8,
        rm: u8,
    },
    /// VFP data processing: destination and source registers (single
    /// precision numbers; double registers as 64 + Dn).
    Fp {
        dst: u8,
        src: [u8; 3],
    },
}

#[derive(Clone, Copy, Debug)]
struct Insn {
    len: u32,
    cycles: u32,
    kind: Kind,
    branch: bool,
    jump: bool,
    store: bool,
    side_effects: bool,
}

impl Insn {
    fn new(len: u32, cycles: u32) -> Self {
        Self { len, cycles, kind: Kind::Other, branch: false, jump: false, store: false, side_effects: false }
    }
    fn kind(mut self, kind: Kind) -> Self {
        self.store |= kind == Kind::Store;
        self.kind = kind;
        self
    }
    /// Always writes the PC: includes the pipeline refill.
    fn branch(mut self) -> Self {
        self.cycles += 1;
        self.jump = true;
        self
    }
    /// Conditional branch: the refill is added when it is taken.
    fn cond_branch(mut self) -> Self {
        self.branch = true;
        self
    }
    fn store(mut self) -> Self {
        self.store = true;
        self
    }
    fn side_effects(mut self) -> Self {
        self.side_effects = true;
        self
    }
}

fn load(len: u32, rn: u16, rt: u16) -> Insn {
    let i = Insn::new(len, 2).kind(Kind::Load { rn: rn as u8, rt: rt as u8 });
    if rt == 15 { i.branch() } else { i }
}

const PC: u16 = 15;
const SP: u16 = 13;

fn decode16(hw: u16) -> Insn {
    let i = Insn::new(2, 1);
    match hw >> 10 {
        0b010001 => {
            // Special data processing, BX/BLX.
            if hw & 0xFF87 == 0x4700 {
                return i.branch(); // BX
            }
            if hw & 0xFF87 == 0x4780 {
                return Insn::new(2, 2).branch(); // BLX register (measured 3)
            }
            let rd = ((hw >> 4) & 8) | (hw & 7);
            let op = (hw >> 8) & 3;
            if rd == PC && op != 1 {
                return i.branch(); // ADD/MOV PC
            }
            i
        }
        0b010010 | 0b010011 => load(2, PC, (hw >> 8) & 7), // LDR literal
        0b010100..=0b010111 => {
            // Load/store register offset.
            let (rn, rt) = ((hw >> 3) & 7, hw & 7);
            if (hw >> 9) & 7 < 3 { i.kind(Kind::Store) } else { load(2, rn, rt) }
        }
        0b011000..=0b100011 => {
            // STR/LDR(B) immediate, STRH/LDRH immediate.
            let (rn, rt) = ((hw >> 3) & 7, hw & 7);
            if hw & 0x0800 != 0 { load(2, rn, rt) } else { i.kind(Kind::Store) }
        }
        0b100100..=0b100111 => {
            // STR/LDR SP-relative.
            if hw & 0x0800 != 0 { load(2, SP, (hw >> 8) & 7) } else { i.kind(Kind::Store) }
        }
        0b101100..=0b101111 => {
            if hw & 0xFE00 == 0xB400 {
                // PUSH
                let n = (hw & 0x1FF).count_ones();
                Insn::new(2, 1 + n).kind(Kind::Multiple).store()
            } else if hw & 0xFE00 == 0xBC00 {
                // POP
                let n = (hw & 0x1FF).count_ones();
                let i = Insn::new(2, 1 + n).kind(Kind::Multiple);
                if hw & 0x100 != 0 { i.branch() } else { i }
            } else if hw & 0xF500 == 0xB100 {
                i.cond_branch() // CBZ/CBNZ
            } else if hw & 0xFFE0 == 0xB660 {
                i.side_effects() // CPSIE/CPSID
            } else {
                i
            }
        }
        0b110000 | 0b110001 => {
            // STM
            let n = (hw & 0xFF).count_ones();
            Insn::new(2, 1 + n).kind(Kind::Multiple).store()
        }
        0b110010 | 0b110011 => Insn::new(2, 1 + (hw & 0xFF).count_ones()).kind(Kind::Multiple), // LDM
        0b110100..=0b110111 => match (hw >> 8) & 0xF {
            0xE => i,                // UDF
            0xF => i.side_effects(), // SVC
            _ => i.cond_branch(),    // B<cond>
        },
        0b111000 | 0b111001 => i.branch(), // B
        _ => i,
    }
}

fn decode32(hw1: u16, hw2: u16) -> Insn {
    let i = Insn::new(4, 1);
    let rn = hw1 & 0xF;
    if hw1 & 0xFE00 == 0xE800 {
        // Load/store multiple, dual, exclusive, table branch.
        if hw1 & 0xFFF0 == 0xE8D0 && hw2 & 0xFFE0 == 0xF000 {
            return Insn::new(4, 2).branch(); // TBB/TBH
        }
        let is_load = hw1 & 0x0010 != 0;
        if hw1 & 0x0040 == 0 && hw1 & 0x0180 != 0 {
            // LDM/STM (IA/DB)
            let n = hw2.count_ones();
            let i = Insn::new(4, 1 + n).kind(Kind::Multiple);
            return match (is_load, hw2 & 0x8000 != 0) {
                (false, _) => i.store(),
                (true, true) => i.branch(),
                (true, false) => i,
            };
        }
        if hw1 & 0x0040 != 0 && hw1 & 0x0120 != 0 {
            // LDRD/STRD
            let i = Insn::new(4, 3).kind(Kind::Multiple);
            return if is_load { i } else { i.store() };
        }
        // LDREX/STREX and friends
        return if is_load { load(4, rn, hw2 >> 12) } else { Insn::new(4, 2).kind(Kind::Store) };
    }
    if hw1 & 0xEC00 == 0xEC00 {
        // Coprocessor / floating point
        let fp_regs = |imm: u16| {
            if hw2 & 0x0100 != 0 { u32::from(imm / 2) } else { u32::from(imm) }
        };
        if hw1 & 0xFE00 == 0xEC00 {
            // 0xEC00..0xEDFF: VLDM/VSTM/VPUSH/VPOP, VLDR/VSTR, 64-bit transfers
            let is_load = hw1 & 0x0010 != 0;
            let i = if hw1 & 0xFF20 == 0xED00 {
                // VLDR/VSTR
                if is_load {
                    Insn::new(4, 2).kind(Kind::Load { rn: rn as u8, rt: 16 })
                } else {
                    Insn::new(4, 1).kind(Kind::Store)
                }
            } else if hw1 & 0xFFE0 == 0xEC40 {
                Insn::new(4, 2) // VMOV two core registers
            } else {
                let n = fp_regs(hw2 & 0xFF);
                let i = Insn::new(4, 1 + n).kind(Kind::Multiple);
                if is_load { i } else { i.store() }
            };
            return i.side_effects();
        }
        if hw1 & 0xFF00 == 0xEE00 && hw2 & 0x0E00 == 0x0A00 {
            if hw2 & 0x0010 != 0 {
                return i.side_effects(); // VMOV core <-> single, VMRS/VMSR
            }
            let opc1 = (hw1 >> 4) & 0xB;
            let cycles = match opc1 {
                0x8 => 14,                                             // VDIV
                0x0 | 0x1 | 0x9 | 0xA => 3,                            // VMLA/VMLS, VNMLA/VNMLS, VFNMA/VFNMS, VFMA/VFMS
                0xB if hw1 & 0xF == 1 && hw2 & 0x00C0 == 0x00C0 => 14, // VSQRT
                _ => 1,
            };
            let double = hw2 & 0x0100 != 0;
            let reg =
                |v: u16, bit: u16| -> u8 { if double { 64 + ((bit << 4) | v) as u8 } else { ((v << 1) | bit) as u8 } };
            let dst = reg((hw2 >> 12) & 0xF, (hw1 >> 6) & 1);
            let n = reg(hw1 & 0xF, (hw2 >> 7) & 1);
            let m = reg(hw2 & 0xF, (hw2 >> 5) & 1);
            let src = match opc1 {
                0x0 | 0x1 | 0x9 | 0xA => [n, m, dst], // multiply-accumulate also reads Sd
                0xB => [m, m, m],                     // one source operand (VSQRT, VABS, VCVT, ...)
                _ => [n, m, m],
            };
            return Insn::new(4, cycles).kind(Kind::Fp { dst, src }).side_effects();
        }
        return i.side_effects(); // other coprocessor instructions
    }
    if hw1 & 0xF800 == 0xF000 && hw2 & 0x8000 != 0 {
        // Branches and miscellaneous control
        return match hw2 & 0x5000 {
            0x0000 if hw1 & 0x0380 == 0x0380 => {
                // MSR, MRS, hints, barriers
                if hw1 & 0xFFE0 == 0xF380 { i.side_effects() } else { i }
            }
            0x0000 => i.cond_branch(), // B<cond>.W
            _ => i.branch(),           // B.W, BL
        };
    }
    if hw1 & 0xFE00 == 0xF800 {
        // Load/store single
        let rt = hw2 >> 12;
        if hw1 & 0x0010 == 0 {
            return i.kind(Kind::Store);
        }
        if rt == PC && hw1 & 0x0040 == 0 {
            return i; // PLD/PLI (byte/halfword loads to PC are hints)
        }
        return load(4, rn, rt);
    }
    if hw1 & 0xFFF0 == 0xFBB0 && hw2 & 0x00F0 == 0x00F0 {
        return i.kind(Kind::Div { signed: false, rn: rn as u8, rm: (hw2 & 0xF) as u8 });
    }
    if hw1 & 0xFFF0 == 0xFB90 && hw2 & 0x00F0 == 0x00F0 {
        return i.kind(Kind::Div { signed: true, rn: rn as u8, rm: (hw2 & 0xF) as u8 });
    }
    i
}

fn decode(code: &[u8]) -> Option<Insn> {
    let hw1 = u16::from_le_bytes([*code.first()?, *code.get(1)?]);
    if hw1 >> 11 >= 0b11101 {
        let hw2 = u16::from_le_bytes([*code.get(2)?, *code.get(3)?]);
        Some(decode32(hw1, hw2))
    } else {
        Some(decode16(hw1))
    }
}

/// Analyses the block at `base`. Division instructions are appended to `divs`.
pub fn analyze_block(code: &[u8], base: u32, divs: &mut Vec<DivSite>) -> BlockTiming {
    let mut t = BlockTiming::default();
    let mut off = 0usize;
    let mut prev = Kind::Other;
    while off < code.len() {
        let Some(insn) = decode(&code[off..]) else {
            break;
        };
        if off == 0 {
            if insn.len == 4 && base % 4 == 2 {
                t.entry_penalty = 1;
            }
            if let Kind::Load { rn, .. } = insn.kind {
                t.first_load_rn = Some(rn);
            }
            if let Kind::Fp { src, .. } = insn.kind {
                t.first_fp_src = Some(src);
            }
        }
        let mut cycles = insn.cycles;
        match insn.kind {
            Kind::Load { rn, .. } => {
                // Pipelined with a preceding load, unless the address depends
                // on its result (not with a preceding store: STR; LDR = 3).
                let pipelined = matches!(prev, Kind::Load { rt, .. } if rt != rn);
                if pipelined {
                    cycles -= 1;
                }
            }
            Kind::Div { signed, rn, rm } => divs.push(DivSite { addr: base + off as u32, signed, rn, rm }),
            Kind::Fp { src, .. } => {
                // The result of a VFP operation is available one cycle late.
                if let Kind::Fp { dst, .. } = prev
                    && src.contains(&dst)
                {
                    cycles += 1;
                }
            }
            _ => {}
        }
        prev = insn.kind;
        t.insns += 1;
        t.cycles += cycles;
        t.stores |= insn.store;
        t.side_effects |= insn.side_effects;
        t.ends_with_branch = insn.branch;
        t.ends_with_jump = insn.jump;
        t.last_load_rt = match insn.kind {
            Kind::Load { rt, .. } => Some(rt),
            _ => None,
        };
        t.last_fp_dst = match insn.kind {
            Kind::Fp { dst, .. } => Some(dst),
            _ => None,
        };
        off += insn.len as usize;
    }
    t
}

/// Duration of UDIV/SDIV: the divider terminates early depending on the
/// number of quotient bits (4 per cycle). Measured on the STM32F429 for all
/// powers of two as dividend and several divisors.
pub fn div_cycles(signed: bool, dividend: u32, divisor: u32) -> u32 {
    let (n, m) =
        if signed { ((dividend as i32).unsigned_abs(), (divisor as i32).unsigned_abs()) } else { (dividend, divisor) };
    if n == 0 || m == 0 {
        return 2;
    }
    let bits = m.leading_zeros() as i32 - n.leading_zeros() as i32;
    if bits < 0 { 3 } else { 5 + bits as u32 / 4 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(words: &[u16]) -> BlockTiming {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        analyze_block(&code, 0x0800_0000, &mut Vec::new())
    }

    // Loop body of the HwCheck kernels: 10 x body, `subs r0, r0, #1`, `bne`.
    const SUBS: u16 = 0x3801;
    const BNE: u16 = 0xD1FE;

    fn kernel(body: &[u16]) -> u32 {
        let mut words = Vec::new();
        for _ in 0..10 {
            words.extend_from_slice(body);
        }
        words.extend_from_slice(&[SUBS, BNE]);
        let t = block(&words);
        assert!(t.ends_with_branch);
        t.cycles + 1 // the backward branch is taken
    }

    #[test]
    fn kernels_match_the_board() {
        // Cycles per loop iteration measured with DWT->CYCCNT (rounded).
        assert_eq!(kernel(&[]), 3);
        assert_eq!(kernel(&[0x1C49]), 13); // adds r1, r1, #1
        assert_eq!(kernel(&[0x4349]), 13); // muls r1, r1, r1
        assert_eq!(kernel(&[0x6811]), 14); // ldr r1, [r2]
        assert_eq!(kernel(&[0x6811, 0x6853]), 24); // ldr r1, [r2]; ldr r3, [r2, #4]
        assert_eq!(kernel(&[0x6811, 0x1C49]), 33); // ldr r1, [r2]; adds r1, #1
        assert_eq!(kernel(&[0x6813, 0x6819]), 34); // ldr r3, [r2]; ldr r1, [r3]
        assert_eq!(kernel(&[0x6011]), 13); // str r1, [r2]
        assert_eq!(kernel(&[0x6019, 0x685C]), 33); // str r1, [r3]; ldr r4, [r3, #4]
        assert_eq!(kernel(&[0x7811]), 14); // ldrb r1, [r2]
        assert_eq!(kernel(&[0xB4F0, 0xBCF0]), 103); // push {r4-r7}; pop {r4-r7}
        assert_eq!(kernel(&[0xCAF0]), 53); // ldm r2, {r4-r7}
        assert_eq!(kernel(&[0xBF08, 0x3101]), 23); // it eq; addeq r1, #1
        assert_eq!(kernel(&[0xEE30, 0x0A20]), 22); // vadd.f32 s0, s0, s1 (dependent)
        assert_eq!(kernel(&[0xEE30, 0x1A20]), 13); // vadd.f32 s2, s0, s1
        assert_eq!(kernel(&[0xEE20, 0x0A20]), 22); // vmul.f32 s0, s0, s1 (dependent)
        // Board: 150 (the TRM's 14 cycles plus a stall that is not always a full cycle).
        assert_eq!(kernel(&[0xEE80, 0x0A20]), 152); // vdiv.f32 s0, s0, s1
        assert_eq!(kernel(&[0xEEB1, 0x0AC0]), 152); // vsqrt.f32 s0, s0
        assert_eq!(kernel(&[0xED92, 0x0A00]), 14); // vldr s0, [r2]
    }

    #[test]
    fn branches() {
        // Unconditional: refill included. Measured: b to the next instruction 2,
        // bl + bx lr 4, blx r3 + bx lr 5.
        assert_eq!(block(&[0xE7FF]).cycles, 2); // b.n next
        assert_eq!(block(&[0xF000, 0xB800]).cycles, 2); // b.w next
        assert_eq!(block(&[0x4770]).cycles, 2); // bx lr
        assert_eq!(block(&[0xF000, 0xF800]).cycles, 2); // bl
        assert_eq!(block(&[0x4798]).cycles, 3); // blx r3
        assert_eq!(block(&[0xBD10]).cycles, 4); // pop {r4, pc}
        assert_eq!(block(&[0xF8DF, 0xF004]).cycles, 3); // ldr.w pc, [pc, #4]
        assert_eq!(block(&[0xE8DF, 0xF000]).cycles, 3); // tbb [pc, r0]
        for b in [[0xE7FFu16].as_slice(), &[0x4770], &[0xF000, 0xF800], &[0xBD10]] {
            assert!(!block(b).ends_with_branch);
        }
        // Conditional: +1 when taken, added at run time.
        assert!(block(&[0xD1FE]).ends_with_branch); // bne
        assert!(block(&[0xB108]).ends_with_branch); // cbz r0, ...
        assert!(block(&[0xF47F, 0xAFFE]).ends_with_branch); // bne.w
        assert_eq!(block(&[0xD1FE]).cycles, 1);
        assert!(!block(&[0x1C49]).ends_with_branch);
    }

    #[test]
    fn loads_pipeline_across_split_blocks() {
        let first = block(&[0x6811, 0x6811]); // ldr r1, [r2] (x2)
        let second = block(&[0x6811, 0x3801]); // ldr r1, [r2]; subs
        assert_eq!(first.cycles + second.cycles, 6);
        assert_eq!(second.continuation_saving(first.last_load_rt), 1);
        let dependent = block(&[0x6809]); // ldr r1, [r1]
        assert_eq!(dependent.continuation_saving(first.last_load_rt), 0);
    }

    #[test]
    fn unaligned_32_bit_branch_target() {
        let code = [0x00u8, 0xF1, 0x01, 0x01]; // add.w r1, r1, #1
        assert_eq!(analyze_block(&code, 0x0800_0002, &mut Vec::new()).entry_penalty, 1);
        assert_eq!(analyze_block(&code, 0x0800_0004, &mut Vec::new()).entry_penalty, 0);
        assert_eq!(block(&[0xBF00]).entry_penalty, 0);
    }

    #[test]
    fn divisions_are_reported() {
        let mut divs = Vec::new();
        let code: Vec<u8> = [0x2000u16, 0xFBB1, 0xF4F2, 0xFB91, 0xF0F3].iter().flat_map(|w| w.to_le_bytes()).collect(); // movs r0, #0; udiv r4, r1, r2; sdiv r0, r1, r3
        let t = analyze_block(&code, 0x100, &mut divs);
        assert_eq!(t.insns, 3);
        assert_eq!(
            divs,
            [DivSite { addr: 0x102, signed: false, rn: 1, rm: 2 }, DivSite { addr: 0x106, signed: true, rn: 1, rm: 3 }]
        );
    }

    #[test]
    fn division_duration_matches_the_board() {
        // (dividend, divisor, UDIV cycles) from HwCheck `div`
        for (n, m, c) in [
            (0, 7, 2),
            (2, 7, 3),
            (4, 7, 5),
            (64, 7, 6),
            (100, 7, 6),
            (2, 1, 5),
            (16, 1, 6),
            (256, 1, 7),
            (0xFFFF_FFFF, 1, 12),
            (0xFFFF_FFFF, 7, 12),
            (0xFFFF_FFFF, 255, 11),
            (0x8000, 0xFFFF, 5),
            (0x8000_0000, 0xFFFF, 9),
            (0x4000_0000, 0x7FFF_FFFF, 5),
            (0x2000_0000, 0x7FFF_FFFF, 3),
        ] {
            assert_eq!(div_cycles(false, n, m), c, "{n} / {m}");
        }
        assert_eq!(div_cycles(true, 1 << 20, (-7i32) as u32), 9);
        assert_eq!(div_cycles(true, u32::MAX, (-7i32) as u32), 3); // -1 / -7
    }
}
