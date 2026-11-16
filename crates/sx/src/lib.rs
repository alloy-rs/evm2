//! Stack superinstructions for evm2.
//!
//! A superinstruction runs a fixed sequence of ordinary instructions with one dispatch. It works on
//! the operand stack like the sequence does, charges the sequence's static gas, and reads the
//! sequence's immediates where they are in the code, so the code keeps its layout: every program
//! counter, jump destination and `PC` value is unchanged.
//!
//! [`translate`] runs as part of bytecode analysis. It walks the instructions once, matches
//! [`PATTERNS`] longest first, and writes each match's [`Sx::slot`] over the first byte of the
//! match in a copy of the code. The interpreter runs that copy on a table whose slot entries run
//! the sequences; the original code, which `CODECOPY` and inspectors see, is unchanged.
//!
//! Superinstructions are internal: slots are opcodes the configuration leaves undefined, and the
//! translation writes a spare undefined opcode over every instruction of the original code whose
//! byte is a slot in use, so such code halts exactly as before. A sequence is only fused when every
//! opcode in it is enabled, so translated code never runs an opcode the configuration does not
//! have.
//!
//! Translation walks the instructions the way jump destination analysis does, with PUSH as the only
//! instruction with an immediate, and marks the jump destinations in the same pass. A configuration
//! that enables an instruction with another immediate (EIP-8024's DUPN, SWAPN and EXCHANGE) fuses
//! nothing, since execution and analysis then disagree on where instructions start.

#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

use alloc::boxed::Box;

/// Opcodes used by [`PATTERNS`].
#[allow(missing_docs)]
pub mod op {
    pub const ADD: u8 = 0x01;
    pub const MUL: u8 = 0x02;
    pub const SUB: u8 = 0x03;
    pub const LT: u8 = 0x10;
    pub const GT: u8 = 0x11;
    pub const SGT: u8 = 0x13;
    pub const EQ: u8 = 0x14;
    pub const ISZERO: u8 = 0x15;
    pub const AND: u8 = 0x16;
    pub const SHL: u8 = 0x1B;
    pub const SHR: u8 = 0x1C;
    pub const POP: u8 = 0x50;
    pub const MLOAD: u8 = 0x51;
    pub const MSTORE: u8 = 0x52;
    pub const JUMP: u8 = 0x56;
    pub const JUMPI: u8 = 0x57;
    pub const JUMPDEST: u8 = 0x5B;
    pub const PUSH0: u8 = 0x5F;
    pub const PUSH1: u8 = 0x60;
    pub const PUSH2: u8 = 0x61;
    pub const PUSH4: u8 = 0x63;
    pub const PUSH20: u8 = 0x73;
    pub const PUSH32: u8 = 0x7F;
    pub const DUP1: u8 = 0x80;
    pub const DUP2: u8 = 0x81;
    pub const DUP3: u8 = 0x82;
    pub const DUP4: u8 = 0x83;
    pub const SWAP1: u8 = 0x90;
    pub const SWAP2: u8 = 0x91;
    pub const SWAP3: u8 = 0x92;
    pub const SWAP4: u8 = 0x93;
    pub const DUPN: u8 = 0xE6;
    pub const SWAPN: u8 = 0xE7;
    pub const EXCHANGE: u8 = 0xE8;
    pub const INVALID: u8 = 0xFE;
}

/// A superinstruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sx {
    /// The opcode byte translated code dispatches it under.
    pub slot: u8,
    /// The opcodes it runs, first opcode in the low byte. See [`len`] and [`op_at`].
    pub ops: u64,
}

impl Sx {
    /// Returns `(input, growth)`: the stack must hold `input` words for the sequence to run, and
    /// grows by at most `growth` words above its starting height while it runs.
    pub const fn stack_bounds(self) -> (usize, usize) {
        let (mut height, mut min, mut max) = (0isize, 0isize, 0isize);
        let mut i = 0;
        while i < self.len() {
            let (input, output) = stack_io(self.op(i));
            height -= input as isize;
            if height < min {
                min = height;
            }
            height += output as isize;
            if height > max {
                max = height;
            }
            i += 1;
        }
        (-min as usize, max as usize)
    }

    /// Returns the number of opcodes it runs.
    #[expect(clippy::len_without_is_empty, reason = "a superinstruction runs 2 to 8 opcodes")]
    #[inline]
    pub const fn len(self) -> usize {
        len(self.ops)
    }

    /// Returns its `i`th opcode.
    #[inline]
    pub const fn op(self, i: usize) -> u8 {
        op_at(self.ops, i)
    }
}

/// Returns the number of opcodes in packed sequence `ops`.
#[inline]
pub const fn len(ops: u64) -> usize {
    8 - ops.leading_zeros() as usize / 8
}

/// Returns opcode `i` of packed sequence `ops`.
#[inline]
pub const fn op_at(ops: u64, i: usize) -> u8 {
    (ops >> (8 * i)) as u8
}

/// Returns the length of the instruction `op` starts, immediate included, with PUSH as the only
/// instruction with an immediate.
#[inline(always)]
pub const fn instruction_len(op: u8) -> usize {
    match op {
        op::PUSH1..=op::PUSH32 => (op - op::PUSH1) as usize + 2,
        _ => 1,
    }
}

/// Returns whether `op` is a slot of [`PATTERNS`].
#[inline(always)]
pub const fn is_slot(op: u8) -> bool {
    op >= SLOT_BASE && op - SLOT_BASE < PATTERNS.len() as u8
}

/// Returns the stack words an opcode of [`PATTERNS`] takes and leaves.
const fn stack_io(op: u8) -> (usize, usize) {
    match op {
        op::POP | op::JUMP => (1, 0),
        op::JUMPI | op::MSTORE => (2, 0),
        op::ISZERO | op::MLOAD => (1, 1),
        op::JUMPDEST => (0, 0),
        op::PUSH0..=op::PUSH32 => (0, 1),
        op::DUP1..=op::DUP4 => ((op - op::DUP1) as usize + 1, (op - op::DUP1) as usize + 2),
        op::SWAP1..=op::SWAP4 => ((op - op::SWAP1) as usize + 2, (op - op::SWAP1) as usize + 2),
        op::ADD
        | op::MUL
        | op::SUB
        | op::LT
        | op::GT
        | op::SGT
        | op::EQ
        | op::AND
        | op::SHL
        | op::SHR => (2, 1),
        _ => panic!("opcode not used by patterns"),
    }
}

const fn pack(ops: &[u8]) -> u64 {
    assert!(ops.len() >= 2 && ops.len() <= 8, "a superinstruction runs 2 to 8 opcodes");
    let mut packed = 0;
    let mut i = 0;
    while i < ops.len() {
        let op = ops[i];
        assert!(op != 0, "STOP cannot be fused");
        assert!(op != op::JUMPDEST || i == 0, "JUMPDEST can only start a superinstruction");
        assert!(
            (op != op::JUMP && op != op::JUMPI) || i == ops.len() - 1,
            "JUMP and JUMPI can only end a superinstruction"
        );
        assert!(!matches!(op, op::DUPN | op::SWAPN | op::EXCHANGE), "only PUSH immediates");
        let _ = stack_io(op);
        packed |= (op as u64) << (8 * i);
        i += 1;
    }
    packed
}

/// First slot. `SLOT_BASE..SLOT_BASE + PATTERNS.len()` are undefined in every Ethereum spec.
const SLOT_BASE: u8 = 0xB0;

macro_rules! patterns {
    ($([$($op:ident)+],)*) => {{
        let pats = [$(pack(&[$(op::$op),+]),)*];
        let mut out = [Sx { slot: 0, ops: 0 }; [$(stringify!($($op)+),)*].len()];
        let mut i = 0;
        while i < pats.len() {
            out[i] = Sx { slot: SLOT_BASE + i as u8, ops: pats[i] };
            i += 1;
        }
        out
    }};
}

/// The superinstructions, in the order a greedy search picked them on 12,700 mainnet blocks
/// spread over blocks 25,428,996 to 26,427,495 (18.5 billion executed instructions), each one
/// saving the most dispatches given the ones before it.
pub const PATTERNS: [Sx; 48] = patterns![
    [PUSH2 JUMPI],
    [DUP1 PUSH4 EQ PUSH2 JUMPI],
    [PUSH2 JUMP],
    [ISZERO PUSH2 JUMPI],
    [JUMPDEST PUSH1],
    [PUSH1 PUSH1 SHL SUB],
    [POP POP],
    [MSTORE PUSH1],
    [SWAP1 POP],
    [SWAP3 SWAP2 POP POP JUMP],
    [PUSH1 ADD],
    [JUMPDEST PUSH2],
    [DUP3 ADD],
    [SWAP1 JUMP],
    [DUP2 MSTORE PUSH1 ADD],
    [DUP2 LT ISZERO PUSH2 JUMPI],
    [DUP2 ADD],
    [JUMPDEST POP],
    [SWAP2 SWAP1],
    [POP JUMP],
    [SWAP1 DUP2],
    [DUP1 PUSH4 GT PUSH2 JUMPI],
    [PUSH20 AND],
    [PUSH1 PUSH1 PUSH1 SHL SUB],
    [PUSH1 MLOAD],
    [JUMPDEST PUSH1 DUP2 SGT ISZERO PUSH2 JUMPI],
    [SWAP1 DUP1 MUL PUSH1 DUP2 SWAP1 SHR],
    [DUP2 MUL SWAP4 SWAP1 SWAP4 ADD SWAP3],
    [JUMPDEST PUSH0],
    [DUP2 LT PUSH2 JUMPI],
    [PUSH1 SHR],
    [PUSH1 PUSH1],
    [SWAP4 SWAP3 POP POP POP JUMP],
    [DUP4 ADD],
    [DUP2 EQ PUSH2 JUMPI],
    [JUMPDEST DUP1],
    [GT ISZERO PUSH2 JUMPI],
    [JUMPDEST DUP2],
    [GT PUSH2 JUMPI],
    [SWAP2 POP],
    [PUSH2 DUP3 PUSH2 JUMP],
    [PUSH1 SWAP1 DUP2 MSTORE PUSH1 PUSH1],
    [DUP1 DUP3],
    [ADD PUSH2 JUMP],
    [MLOAD DUP4 DUP3 ADD MSTORE PUSH1],
    [DUP2 AND],
    [JUMPDEST SWAP1],
    [PUSH1 SHL],
];

const _: () = {
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut j = 0;
        while j < i {
            assert!(PATTERNS[i].ops != PATTERNS[j].ops, "duplicate pattern");
            j += 1;
        }
        i += 1;
    }
};

/// A set of opcodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OpSet([u64; 4]);

impl OpSet {
    /// The empty set.
    pub const EMPTY: Self = Self([0; 4]);

    /// Returns whether `op` is in the set.
    #[inline]
    pub const fn contains(&self, op: u8) -> bool {
        self.0[op as usize / 64] >> (op % 64) & 1 != 0
    }

    /// Adds `op` to the set.
    #[inline]
    pub const fn insert(&mut self, op: u8) {
        self.0[op as usize / 64] |= 1 << (op % 64);
    }
}

/// The opcodes of a configuration, as far as superinstructions are concerned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ops {
    /// Opcodes that are enabled and run without hooks, so they may be fused.
    pub fusable: OpSet,
    /// Opcodes the configuration leaves undefined, so they may be slots.
    pub free: OpSet,
}

impl Ops {
    /// Returns whether `sx` applies: PUSH is the only enabled instruction with an immediate, its
    /// slot is free and every opcode it runs is fusable.
    #[inline]
    pub const fn is_active(&self, sx: Sx) -> bool {
        if !self.immediates_only_on_push() || !self.free.contains(sx.slot) {
            return false;
        }
        let mut i = 0;
        while i < sx.len() {
            if !self.fusable.contains(sx.op(i)) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Returns whether PUSH is the only enabled instruction with an immediate, so the instruction
    /// boundaries execution follows are those of jump destination analysis.
    #[inline]
    pub const fn immediates_only_on_push(&self) -> bool {
        !self.fusable.contains(op::DUPN)
            && !self.fusable.contains(op::SWAPN)
            && !self.fusable.contains(op::EXCHANGE)
    }
}

/// Bytes a pattern window covers. Every pattern's opcodes and immediates fit in it.
const WINDOW: usize = 24;

/// Bytes past an instruction the fast path may read: the window, or the opcode after a PUSH32.
const LOOKAHEAD: usize = 33;

/// First opcodes of [`PATTERNS`], each once, in pattern order.
const FIRSTS: ([u8; PATTERNS.len()], usize) = {
    let mut firsts = [0; PATTERNS.len()];
    let mut n = 0;
    let mut i = 0;
    while i < PATTERNS.len() {
        let op = PATTERNS[i].op(0);
        let mut j = 0;
        while j < n && firsts[j] != op {
            j += 1;
        }
        if j == n {
            firsts[n] = op;
            n += 1;
        }
        i += 1;
    }
    (firsts, n)
};

/// Number of distinct first opcodes, plus one for "none".
const GROUPS: usize = FIRSTS.1 + 1;

/// A pattern as byte constraints on the code at its first opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window {
    mask: [u64; 3],
    value: [u64; 3],
    span: u8,
    slot: u8,
}

impl Window {
    const NONE: Self = Self { mask: [0; 3], value: [0; 3], span: 0, slot: 0 };

    const fn of(sx: Sx) -> Self {
        let mut w = Self { span: 0, slot: sx.slot, ..Self::NONE };
        let mut offset = 0;
        let mut i = 0;
        while i < sx.len() {
            let op = sx.op(i);
            w.mask[offset / 8] |= 0xFF << (8 * (offset % 8));
            w.value[offset / 8] |= (op as u64) << (8 * (offset % 8));
            offset += instruction_len(op);
            i += 1;
        }
        assert!(offset <= WINDOW, "pattern longer than the window");
        w.span = offset as u8;
        w
    }

    /// Returns whether the pattern matches the window `w`, read little endian at its first opcode.
    #[inline(always)]
    const fn matches(&self, w: &[u64; 3]) -> bool {
        ((w[0] & self.mask[0]) ^ self.value[0])
            | ((w[1] & self.mask[1]) ^ self.value[1])
            | ((w[2] & self.mask[2]) ^ self.value[2])
            == 0
    }
}

/// Opcode entry flags for the translation loop, above the group in the low byte.
const JUMPDEST: u16 = 1 << 8;
const SLOT: u16 = 1 << 9;

/// The superinstructions of a configuration, prepared for [`translate`].
///
/// Candidates are indexed by their first two opcodes, longest first, so most instructions are
/// rejected with one table load.
#[derive(Clone, Debug)]
pub struct Matcher {
    ops: Ops,
    /// By opcode: the group of a first opcode of an active pattern in the low byte (0 for none),
    /// and [`JUMPDEST`] and [`SLOT`]. Zero for every other opcode.
    entry: [u16; 256],
    /// Candidates by group and second opcode: start in [`Self::windows`] in the low byte, count
    /// above.
    pairs: [[u16; 256]; GROUPS],
    windows: [Window; PATTERNS.len()],
    filler: u8,
    active: bool,
}

impl Matcher {
    /// Prepares the superinstructions that apply with `ops`; see [`Ops::is_active`].
    pub const fn new(ops: Ops) -> Self {
        let mut m = Self {
            ops,
            entry: [0; 256],
            pairs: [[0; 256]; GROUPS],
            windows: [Window::NONE; PATTERNS.len()],
            filler: op::INVALID,
            active: false,
        };
        m.entry[op::JUMPDEST as usize] = JUMPDEST;
        let mut slots = OpSet::EMPTY;
        let mut i = 0;
        while i < PATTERNS.len() {
            if ops.is_active(PATTERNS[i]) {
                slots.insert(PATTERNS[i].slot);
                m.entry[PATTERNS[i].slot as usize] |= SLOT;
                m.active = true;
            }
            i += 1;
        }
        // An undefined opcode that is no slot in use halts like any undefined opcode.
        let mut op = 0;
        while op < 256 {
            if ops.free.contains(op as u8) && !slots.contains(op as u8) {
                m.filler = op as u8;
                break;
            }
            op += 1;
        }
        // Windows grouped by (first, second) opcode, longest first within a pair.
        let mut n = 0;
        let mut g = 0;
        while g < FIRSTS.1 {
            let first = FIRSTS.0[g];
            let mut i = 0;
            while i < PATTERNS.len() {
                let second = PATTERNS[i].op(1);
                if PATTERNS[i].op(0) == first
                    && ops.is_active(PATTERNS[i])
                    && m.pairs[g + 1][second as usize] == 0
                {
                    let start = n;
                    let mut len = 8;
                    while len >= 2 {
                        let mut j = 0;
                        while j < PATTERNS.len() {
                            let sx = PATTERNS[j];
                            if sx.len() == len
                                && sx.op(0) == first
                                && sx.op(1) == second
                                && ops.is_active(sx)
                            {
                                m.windows[n] = Window::of(sx);
                                n += 1;
                            }
                            j += 1;
                        }
                        len -= 1;
                    }
                    m.entry[first as usize] |= g as u16 + 1;
                    m.pairs[g + 1][second as usize] = (start as u16) | (((n - start) as u16) << 8);
                }
                i += 1;
            }
            g += 1;
        }
        m
    }

    /// Returns the opcodes it was prepared for.
    #[inline]
    pub const fn ops(&self) -> &Ops {
        &self.ops
    }

    /// Returns whether any superinstruction applies.
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the slot of the longest pattern that matches at instruction `pc`, with its span.
    ///
    /// # Safety
    ///
    /// `code` must hold [`LOOKAHEAD`] bytes past `pc`.
    #[inline(always)]
    unsafe fn find(&self, code: *const u8, pc: usize, op: u8, g: usize) -> Option<(u8, usize)> {
        // SAFETY: An instruction is at most 33 bytes, and the window 24.
        unsafe {
            let second = *code.add(pc + instruction_len(op));
            let pair = self.pairs[g][second as usize];
            let (start, count) = ((pair & 0xFF) as usize, (pair >> 8) as usize);
            if count == 0 {
                return None;
            }
            let p = code.add(pc).cast::<u64>();
            let w = [
                u64::from_le(p.read_unaligned()),
                u64::from_le(p.add(1).read_unaligned()),
                u64::from_le(p.add(2).read_unaligned()),
            ];
            for window in self.windows.get_unchecked(start..start + count) {
                if window.matches(&w) {
                    return Some((window.slot, window.span as usize));
                }
            }
        }
        None
    }
}

/// Translates legacy bytecode to the superinstructions of `matcher`.
///
/// `code` is the padded bytecode and `len` its length before padding. Returns a copy of `code`
/// in which the first byte of each sequence that matches an active pattern holds the pattern's
/// slot, and every other instruction whose opcode is a slot in use holds an undefined opcode that
/// is not a slot in use. Matching is greedy, longest first, one pass over the instructions.
///
/// With `jumpdests`, also marks every `JUMPDEST` instruction in it, one bit per byte of
/// `code[..len]`, least significant bit first, as jump destination analysis does.
pub fn translate(
    code: &[u8],
    len: usize,
    matcher: &Matcher,
    mut jumpdests: Option<&mut [u8]>,
) -> Box<[u8]> {
    assert!(len <= code.len());
    if let Some(jumpdests) = jumpdests.as_deref() {
        assert!(jumpdests.len() >= len.div_ceil(8));
    }
    let mut out: Box<[u8]> = code.into();
    let dst = out.as_mut_ptr();
    let src = code.as_ptr();
    // Instructions below `fast` have the lookahead in bounds.
    let fast = len.min(code.len().saturating_sub(LOOKAHEAD));
    let mut pc = 0;
    // SAFETY: `pc < fast <= len <= code.len() == out.len()`, with the lookahead in bounds, and
    // `jumpdests` has a bit for each byte below `len`.
    unsafe {
        while pc < fast {
            let op = *src.add(pc);
            let entry = matcher.entry[op as usize];
            if entry != 0 {
                if entry & (JUMPDEST | SLOT) != 0 {
                    if entry & SLOT != 0 {
                        *dst.add(pc) = matcher.filler;
                        pc += 1;
                        continue;
                    }
                    if let Some(jumpdests) = jumpdests.as_deref_mut() {
                        *jumpdests.get_unchecked_mut(pc / 8) |= 1 << (pc % 8);
                    }
                }
                let g = (entry & 0xFF) as usize;
                if g != 0
                    && let Some((slot, span)) = matcher.find(src, pc, op, g)
                {
                    *dst.add(pc) = slot;
                    pc += span;
                    continue;
                }
            }
            pc += instruction_len(op);
        }
    }
    while pc < len {
        let op = code[pc];
        let entry = matcher.entry[op as usize];
        if entry & SLOT != 0 {
            out[pc] = matcher.filler;
            pc += 1;
            continue;
        }
        if entry & JUMPDEST != 0
            && let Some(jumpdests) = jumpdests.as_deref_mut()
        {
            jumpdests[pc / 8] |= 1 << (pc % 8);
        }
        match find_slow(code, len, pc, matcher) {
            Some((slot, span)) => {
                out[pc] = slot;
                pc += span;
            }
            None => pc += instruction_len(op),
        }
    }
    out
}

/// [`Matcher::find`] near the end of code without padding.
#[cold]
fn find_slow(code: &[u8], len: usize, pc: usize, matcher: &Matcher) -> Option<(u8, usize)> {
    let mut longest: Option<Sx> = None;
    for sx in PATTERNS {
        if matcher.ops.is_active(sx)
            && sx.op(0) == code[pc]
            && match_at(code, len, pc, sx).is_some()
            && longest.is_none_or(|l| sx.len() > l.len())
        {
            longest = Some(sx);
        }
    }
    longest.map(|sx| (sx.slot, match_at(code, len, pc, sx).unwrap() - pc))
}

/// Returns the end of `sx`'s match at instruction `pc`, if it matches there.
fn match_at(code: &[u8], len: usize, mut pc: usize, sx: Sx) -> Option<usize> {
    for i in 0..sx.len() {
        if pc >= len || code[pc] != sx.op(i) {
            return None;
        }
        pc += instruction_len(code[pc]);
    }
    Some(pc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// All Prague opcodes, nothing hooked; slots free.
    fn prague() -> Ops {
        let mut ops = Ops::default();
        for op in 0..=255u8 {
            let defined = matches!(op, 0x00..=0x0B | 0x10..=0x1D | 0x20 | 0x30..=0x4A | 0x50..=0x9F | 0xA0..=0xA4 | 0xF0..=0xF5 | 0xFA | 0xFD..=0xFF);
            if defined { ops.fusable.insert(op) } else { ops.free.insert(op) }
        }
        ops
    }

    fn reference_jumpdests(code: &[u8]) -> Vec<u8> {
        let mut table = alloc::vec![0u8; code.len().div_ceil(8)];
        let mut pc = 0;
        while pc < code.len() {
            let op = code[pc];
            if op == op::JUMPDEST {
                table[pc / 8] |= 1 << (pc % 8);
            }
            pc += if (op::PUSH1..=op::PUSH32).contains(&op) {
                (op - op::PUSH1) as usize + 2
            } else {
                1
            };
        }
        table
    }

    fn pad(code: &[u8]) -> Vec<u8> {
        let mut v = code.to_vec();
        v.extend_from_slice(&[0; 33]);
        v
    }

    #[test]
    fn slots() {
        for (i, sx) in PATTERNS.iter().enumerate() {
            assert_eq!(sx.slot as usize, 0xB0 + i);
            assert!(prague().free.contains(sx.slot));
            assert!(Window::of(*sx).span as usize <= WINDOW);
        }
        assert!(Matcher::new(prague()).is_active());
    }

    #[test]
    fn fuses_longest_and_keeps_layout() {
        use op::*;
        // DUP1 PUSH4 sel EQ PUSH2 dst JUMPI, then PUSH2 dst JUMPI.
        let code = [DUP1, PUSH4, 1, 2, 3, 4, EQ, PUSH2, 0, 9, JUMPI, PUSH2, 0, 9, JUMPI];
        let padded = pad(&code);
        let out = translate(&padded, code.len(), &Matcher::new(prague()), None);
        assert_eq!(out.len(), padded.len());
        assert_eq!(out[0], PATTERNS[1].slot);
        assert_eq!(&out[1..11], &code[1..11]);
        assert_eq!(out[11], PATTERNS[0].slot);
        assert_eq!(&out[12..], &padded[12..]);
    }

    #[test]
    fn does_not_fuse_disabled_opcodes() {
        use op::*;
        let code = [JUMPDEST, PUSH0, PUSH1, 1, PUSH1, 2, SHL, SUB];
        let padded = pad(&code);
        let mut ops = prague();
        let out = translate(&padded, code.len(), &Matcher::new(ops), None);
        assert_eq!((out[0], out[2]), (PATTERNS[28].slot, PATTERNS[5].slot));
        // Before Shanghai PUSH0 is undefined, and before Constantinople SHL is: only the sequence
        // of enabled opcodes fuses.
        ops.fusable = OpSet::EMPTY;
        for op in [JUMPDEST, PUSH1, SUB] {
            ops.fusable.insert(op);
        }
        let out = translate(&padded, code.len(), &Matcher::new(ops), None);
        assert_eq!((out[0], out[1], out[2]), (JUMPDEST, PUSH0, PATTERNS[31].slot));
        assert_eq!(&out[3..], &padded[3..]);
    }

    #[test]
    fn slot_bytes_in_code_become_invalid() {
        use op::*;
        let slot = PATTERNS[0].slot;
        // A slot as an instruction, and as PUSH data, which is not an instruction.
        let code = [slot, PUSH1, slot, PUSH2, 0, 5, JUMPI];
        let padded = pad(&code);
        let out = translate(&padded, code.len(), &Matcher::new(prague()), None);
        // 0x0C, the first undefined opcode, halts like the slot would without superinstructions.
        assert_eq!(out[0], 0x0C);
        assert_eq!(out[2], slot);
        assert_eq!(out[3], PATTERNS[0].slot);
        // A slot of a pattern that is not active stays as it is.
        let mut ops = prague();
        ops.fusable = OpSet::EMPTY;
        assert_eq!(translate(&padded, code.len(), &Matcher::new(ops), None)[0], slot);
    }

    #[test]
    fn nothing_fuses_with_eip8024() {
        use op::*;
        let code = [PUSH2, 0, 5, JUMPI, PUSH1, 1, ADD];
        let padded = pad(&code);
        let mut ops = prague();
        ops.fusable.insert(DUPN);
        ops.free = OpSet::EMPTY;
        for op in 0xB0..=0xDF {
            ops.free.insert(op);
        }
        assert!(PATTERNS.iter().all(|sx| !ops.is_active(*sx)));
        let mut jumps = alloc::vec![0u8; 1];
        assert_eq!(
            &translate(&padded, code.len(), &Matcher::new(ops), Some(&mut jumps))[..],
            &padded[..]
        );
    }

    #[test]
    fn stack_bounds() {
        use op::*;
        let sx = |ops: &[u8]| Sx { slot: 0, ops: pack(ops) };
        assert_eq!(sx(&[PUSH2, JUMPI]).stack_bounds(), (1, 1));
        assert_eq!(sx(&[DUP1, PUSH4, EQ, PUSH2, JUMPI]).stack_bounds(), (1, 2));
        assert_eq!(sx(&[SWAP3, SWAP2, POP, POP, JUMP]).stack_bounds(), (4, 0));
        assert_eq!(sx(&[PUSH1, PUSH1, PUSH1, SHL, SUB]).stack_bounds(), (0, 3));
        assert_eq!(sx(&[MLOAD, DUP4, DUP3, ADD, MSTORE, PUSH1]).stack_bounds(), (4, 2));
    }

    #[test]
    fn jumpdest_inside_a_sequence_ends_it() {
        use op::*;
        let code = [PUSH1, 1, JUMPDEST, ADD];
        let padded = pad(&code);
        let out = translate(&padded, code.len(), &Matcher::new(prague()), None);
        assert_eq!(&out[..], &padded[..]);
    }

    /// Greedy longest match, one candidate at a time.
    fn reference(code: &[u8], len: usize, ops: &Ops) -> Vec<u8> {
        let m = Matcher::new(*ops);
        let mut out = code.to_vec();
        let mut pc = 0;
        while pc < len {
            if m.entry[code[pc] as usize] & SLOT != 0 {
                out[pc] = m.filler;
                pc += 1;
                continue;
            }
            match find_slow(code, len, pc, &m) {
                Some((slot, span)) => {
                    out[pc] = slot;
                    pc += span;
                }
                None => pc += instruction_len(code[pc]),
            }
        }
        out
    }

    #[test]
    fn matches_reference() {
        let mut s = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let alphabet: Vec<u8> =
            PATTERNS.iter().flat_map(|p| (0..p.len()).map(move |i| p.op(i))).collect();
        let mut ops = prague();
        for round in 0..4000 {
            if round == 2000 {
                // Without SHL, SHR and PUSH0, as before Constantinople and Shanghai.
                for op in [op::SHL, op::SHR, op::PUSH0] {
                    ops.fusable.0[op as usize / 64] &= !(1 << (op % 64));
                    ops.free.insert(op);
                }
            }
            let n = (next() % 400) as usize;
            let code: Vec<u8> = (0..n)
                .map(|_| {
                    let r = next();
                    if r % 5 == 0 {
                        (r >> 24) as u8
                    } else {
                        alphabet[(r >> 8) as usize % alphabet.len()]
                    }
                })
                .collect();
            // Padded, and unpadded code that happens to end in 33 zeros.
            for padded in [pad(&code), {
                let mut c = code.clone();
                c.extend_from_slice(&[0; 33]);
                c
            }] {
                let len = if padded.len() == n + 33 && round % 2 == 1 { padded.len() } else { n };
                let got = translate(&padded, len, &Matcher::new(ops), None);
                assert_eq!(&got[..], &reference(&padded, len, &ops)[..], "round {round}");
            }
        }
    }

    #[test]
    fn random_code() {
        // xorshift, biased towards opcodes that patterns use.
        let mut s = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let alphabet: Vec<u8> =
            PATTERNS.iter().flat_map(|p| (0..p.len()).map(move |i| p.op(i))).collect();
        let ops = prague();
        for _ in 0..2000 {
            let n = (next() % 300) as usize;
            let code: Vec<u8> = (0..n)
                .map(|_| {
                    let r = next();
                    if r % 4 == 0 {
                        r as u8 >> 3 ^ (r >> 16) as u8
                    } else {
                        alphabet[(r >> 8) as usize % alphabet.len()]
                    }
                })
                .collect();
            let padded = pad(&code);
            let mut jumps = alloc::vec![0u8; n.div_ceil(8)];
            let out = translate(&padded, n, &Matcher::new(ops), Some(&mut jumps));
            assert_eq!(jumps, reference_jumpdests(&code));
            assert_eq!(&out[n..], &padded[n..]);
            // Walk the instructions: every changed byte starts a full match of its pattern, or is a
            // slot turned INVALID; nothing changes elsewhere.
            let mut pc = 0;
            while pc < n {
                let (a, b) = (padded[pc], out[pc]);
                if a == b {
                    assert!(!(0xB0..0xB0 + PATTERNS.len() as u8).contains(&b), "slot left in code");
                    pc += instruction_len(a);
                } else if b == 0x0C {
                    assert!(is_slot(a));
                    pc += 1;
                } else {
                    let sx = PATTERNS[(b - 0xB0) as usize];
                    assert_eq!(sx.op(0), a);
                    let end = match_at(&padded, n, pc, sx).expect("no match");
                    for q in pc + 1..end {
                        assert_eq!(out[q], padded[q]);
                    }
                    pc = end;
                }
            }
        }
    }
}
