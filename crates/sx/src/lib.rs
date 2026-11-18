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

use alloc::{boxed::Box, vec::Vec};

/// Opcodes used by [`PATTERNS`].
#[allow(missing_docs)]
pub mod op {
    pub const ADD: u8 = 0x01;
    pub const MUL: u8 = 0x02;
    pub const SUB: u8 = 0x03;
    pub const LT: u8 = 0x10;
    pub const SLT: u8 = 0x12;
    pub const GT: u8 = 0x11;
    pub const SGT: u8 = 0x13;
    pub const EQ: u8 = 0x14;
    pub const ISZERO: u8 = 0x15;
    pub const AND: u8 = 0x16;
    pub const OR: u8 = 0x17;
    pub const XOR: u8 = 0x18;
    pub const NOT: u8 = 0x19;
    pub const SHL: u8 = 0x1B;
    pub const SHR: u8 = 0x1C;
    pub const SAR: u8 = 0x1D;
    pub const CALLVALUE: u8 = 0x34;
    pub const CALLDATALOAD: u8 = 0x35;
    pub const CALLDATASIZE: u8 = 0x36;
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
    pub const PUSH8: u8 = 0x67;
    pub const PUSH20: u8 = 0x73;
    pub const PUSH32: u8 = 0x7F;
    pub const DUP1: u8 = 0x80;
    pub const DUP2: u8 = 0x81;
    pub const DUP3: u8 = 0x82;
    pub const DUP4: u8 = 0x83;
    pub const DUP5: u8 = 0x84;
    pub const DUP8: u8 = 0x87;
    pub const SWAP1: u8 = 0x90;
    pub const SWAP2: u8 = 0x91;
    pub const SWAP3: u8 = 0x92;
    pub const SWAP4: u8 = 0x93;
    pub const SWAP8: u8 = 0x97;
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

    /// Returns the size of the PUSH that supplies the target of a final JUMP or JUMPI, when the
    /// opcode before the jump is that PUSH: the jump goes to a constant, which translation checks.
    pub const fn static_jump(self) -> Option<usize> {
        let n = self.len();
        match (self.op(n - 1), self.op(n - 2)) {
            (op::JUMP | op::JUMPI, push @ op::PUSH1..=op::PUSH32) => {
                Some((push - op::PUSH0) as usize)
            }
            _ => None,
        }
    }

    /// Returns the net change of the stack height when the sequence completes.
    pub const fn stack_delta(self) -> isize {
        let mut delta = 0;
        let mut i = 0;
        while i < self.len() {
            let (input, output) = stack_io(self.op(i));
            delta += output as isize - input as isize;
            i += 1;
        }
        delta
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
    IS_SLOT[op as usize]
}

/// Opcodes superinstructions may use, all undefined from Frontier through Osaka. A pattern only
/// applies where its slot is undefined, so a later spec that defines one turns that pattern off.
const SLOTS: [u8; 105] = {
    let mut slots = [0; 105];
    let mut n = 0;
    let mut op = 0xB0;
    while op <= 0xEF {
        slots[n] = op as u8;
        n += 1;
        op += 1;
    }
    op = 0x21;
    while op <= 0x2F {
        slots[n] = op as u8;
        n += 1;
        op += 1;
    }
    op = 0xA5;
    while op <= 0xAF {
        slots[n] = op as u8;
        n += 1;
        op += 1;
    }
    // 0x0C stays free: translation writes it over slot bytes that are instructions in the code.
    let more =
        [0x0D, 0x0E, 0x0F, 0x1F, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0xF6, 0xF7, 0xF8, 0xF9, 0xFB, 0xFC];
    let mut i = 0;
    while i < more.len() {
        slots[n] = more[i];
        n += 1;
        i += 1;
    }
    assert!(n == 105);
    slots
};

const IS_SLOT: [bool; 256] = {
    let mut is = [false; 256];
    let mut i = 0;
    while i < PATTERNS.len() {
        is[PATTERNS[i].slot as usize] = true;
        i += 1;
    }
    is
};

/// Returns the stack words an opcode of [`PATTERNS`] takes and leaves.
const fn stack_io(op: u8) -> (usize, usize) {
    match op {
        op::POP | op::JUMP => (1, 0),
        op::JUMPI | op::MSTORE => (2, 0),
        op::ISZERO | op::NOT | op::MLOAD | op::CALLDATALOAD => (1, 1),
        op::JUMPDEST => (0, 0),
        op::PUSH0..=op::PUSH32 | op::CALLDATASIZE | op::CALLVALUE => (0, 1),
        op::DUP1..=op::DUP8 => ((op - op::DUP1) as usize + 1, (op - op::DUP1) as usize + 2),
        op::SWAP1..=op::SWAP8 => ((op - op::SWAP1) as usize + 2, (op - op::SWAP1) as usize + 2),
        op::ADD
        | op::MUL
        | op::SUB
        | op::LT
        | op::GT
        | op::SLT
        | op::SGT
        | op::EQ
        | op::AND
        | op::OR
        | op::XOR
        | op::SHL
        | op::SHR
        | op::SAR => (2, 1),
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

macro_rules! patterns {
    ($([$($op:ident)+],)*) => {{
        let pats = [$(pack(&[$(op::$op),+]),)*];
        let mut out = [Sx { slot: 0, ops: 0 }; [$(stringify!($($op)+),)*].len()];
        let mut i = 0;
        while i < pats.len() {
            out[i] = Sx { slot: SLOTS[i], ops: pats[i] };
            i += 1;
        }
        out
    }};
}

/// The superinstructions, in the order a greedy search picked them on 12,700 mainnet blocks
/// spread over blocks 25,428,996 to 26,427,495 (18.5 billion executed instructions), each one
/// saving the most dispatches given the ones before it; four slots whose patterns later, longer
/// ones made unreachable went to the next best candidates. Patterns that start with JUMPDEST are
/// three or more instructions long: a jump always lands on the JUMPDEST handler, which keeps the
/// jump's own dispatch predictable, so fusing a JUMPDEST only pays when it removes two dispatches.
pub const PATTERNS: [Sx; 105] = patterns![
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
    [DUP3 ADD],
    [SWAP1 JUMP],
    [DUP2 MSTORE PUSH1 ADD],
    [DUP2 LT ISZERO PUSH2 JUMPI],
    [DUP2 ADD],
    [SWAP2 SWAP1],
    [POP JUMP],
    [SWAP1 DUP2],
    [DUP1 PUSH4 GT PUSH2 JUMPI],
    [PUSH20 AND],
    [PUSH1 PUSH1 PUSH1 SHL SUB],
    [PUSH1 MLOAD],
    [JUMPDEST PUSH1 DUP2 SGT ISZERO PUSH2 JUMPI],
    [SWAP1 DUP1 MUL PUSH1 DUP2 SWAP1 SHR],
    [DUP2 MSTORE],
    [JUMPDEST PUSH0],
    [DUP2 LT PUSH2 JUMPI],
    [PUSH1 SHR],
    [PUSH1 PUSH1],
    [SWAP4 SWAP3 POP POP POP JUMP],
    [DUP4 ADD],
    [DUP2 EQ PUSH2 JUMPI],
    [GT ISZERO PUSH2 JUMPI],
    [GT PUSH2 JUMPI],
    [SWAP2 POP],
    [PUSH2 DUP3 PUSH2 JUMP],
    [PUSH1 SWAP1 DUP2 MSTORE PUSH1 PUSH1],
    [DUP1 DUP3],
    [ADD PUSH2 JUMP],
    [DUP1 MLOAD],
    [DUP2 AND],
    [PUSH1 SHL],
    // A second greedy pass on top of the patterns above.
    [PUSH2 DUP2 PUSH2 JUMP],
    [JUMPDEST POP POP],
    [PUSH2 SWAP2 SWAP1 PUSH2 JUMP],
    [SWAP1 POP SWAP2 SWAP1 POP JUMP],
    [PUSH1 ADD SHR SWAP1 PUSH1 SAR PUSH2 JUMP],
    [DUP1 SWAP2 SUB SWAP1],
    [PUSH1 DUP1],
    [DUP5 ADD],
    [DUP3 GT OR PUSH2 JUMPI],
    [PUSH1 DUP3 ADD],
    [JUMPDEST DUP4 DUP2 LT ISZERO PUSH2 JUMPI],
    [SWAP1 DUP2 MSTORE PUSH1 ADD PUSH1],
    [POP POP POP POP],
    [PUSH1 PUSH1 MSTORE PUSH1 CALLDATASIZE LT],
    [DUP1 ISZERO PUSH2 JUMPI],
    [PUSH2 SWAP1],
    [SWAP2 ADD],
    [PUSH1 MSTORE],
    [DUP3 AND],
    [SWAP1 SWAP2],
    [MLOAD PUSH1],
    [JUMPDEST PUSH1 DUP1],
    [SUB PUSH2 JUMPI],
    [DUP3 DUP3],
    [PUSH1 NOT],
    [JUMPDEST SWAP3 SWAP2 POP POP JUMP],
    [JUMPDEST SWAP1 POP],
    [LT ISZERO PUSH2 JUMPI],
    [PUSH2 PUSH2],
    [ISZERO ISZERO],
    [SWAP3 POP],
    [JUMPDEST POP PUSH1 ADD SWAP1 JUMP],
    [JUMPDEST PUSH2 PUSH2 JUMP],
    [DUP1 DUP4 SUB DUP2],
    [PUSH2 MLOAD],
    [DUP2 ADD SWAP1 DUP2 LT PUSH8],
    [DUP4 PUSH2 JUMP],
    [SWAP2 POP POP],
    [ADD MLOAD],
    [PUSH2 PUSH1],
    [POP PUSH1],
    [SWAP2 SWAP1 PUSH4 AND JUMP],
    [ADD MLOAD DUP4 DUP3 ADD MSTORE PUSH1],
    [LT PUSH2 JUMPI],
    [EQ PUSH2 JUMPI],
    [JUMPDEST PUSH1 MLOAD],
    [SLT PUSH2 JUMPI],
    // A third pass, for the remaining undefined opcodes.
    [DUP2 DUP2],
    [JUMPDEST PUSH2 JUMP],
    [JUMPDEST PUSH1 PUSH1 PUSH1 SHL SUB],
    [CALLDATALOAD PUSH1 SHR],
    [PUSH1 PUSH1 MSTORE CALLVALUE DUP1 ISZERO PUSH2 JUMPI],
    [JUMPDEST POP PUSH1 CALLDATASIZE LT PUSH2 JUMPI],
    [JUMPDEST SWAP2 SWAP1],
    [PUSH1 DUP3 DUP5 SUB SLT ISZERO PUSH2 JUMPI],
    [DUP2 ADD SWAP1 PUSH2 SWAP2 SWAP1 PUSH2 JUMP],
    [ADD MSTORE],
    [DUP3 DUP2 MUL SWAP4 SWAP1 SWAP4 ADD SWAP3],
    [JUMPDEST DUP1 DUP3 ADD DUP1 DUP3],
    [DUP2 MLOAD],
    [PUSH1 PUSH1 PUSH1 SHL SUB AND],
    [DUP1 CALLDATALOAD],
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
    /// For a constant jump target: the offset of its PUSH immediate, and its size (0 for none).
    target_at: u8,
    target_len: u8,
}

impl Window {
    const NONE: Self =
        Self { mask: [0; 3], value: [0; 3], span: 0, slot: 0, target_at: 0, target_len: 0 };

    const fn of(sx: Sx) -> Self {
        let mut w = Self { span: 0, slot: sx.slot, ..Self::NONE };
        if let Some(n) = sx.static_jump() {
            w.target_at = offsets(sx)[sx.len() - 2] as u8 + 1;
            w.target_len = n as u8;
        }
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
}

/// Candidates compared at once: at least the most patterns that share their first two opcodes.
const LANES: usize = 8;

/// Distinct (first, second) opcode pairs of [`PATTERNS`], plus one for "none" and one for slot
/// opcodes in the code.
const BLOCKS: usize = {
    let mut n = 2;
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut j = 0;
        while j < i
            && !(PATTERNS[j].op(0) == PATTERNS[i].op(0) && PATTERNS[j].op(1) == PATTERNS[i].op(1))
        {
            j += 1;
        }
        if j == i {
            n += 1;
        }
        i += 1;
    }
    n
};

/// The block of slot opcodes in the code: always matches, one byte, writing the filler.
const FILL_BLOCK: usize = BLOCKS - 1;

/// The group of slot opcodes in the code.
const FILL_GROUP: usize = GROUPS;

/// [`Matcher::hash_b`] flags.
const IS_FIRST: u8 = 1 << 3;
const HASH_SLOT: u8 = 1 << 4;

/// Index in [`FIRSTS`] by opcode.
const FIRSTS_INDEX: [u8; 256] = {
    let mut index = [0; 256];
    let mut i = 0;
    while i < FIRSTS.1 {
        index[FIRSTS.0[i] as usize] = i as u8;
        i += 1;
    }
    index
};

/// A first opcode's hashes, from its index in [`FIRSTS`].
const fn first_hashes(op: u8) -> (u8, u8) {
    let g = FIRSTS_INDEX[op as usize] as u32;
    // Multipliers picked so the pairs of [`PATTERNS`] get distinct keys (checked below) with few
    // other pairs of executed code sharing them.
    ((g.wrapping_mul(11) >> 1) as u8 & 0x7F, (g & 7) as u8 | IS_FIRST)
}

/// Returns the 11-bit key of an instruction whose opcode has hashes `hash_a` and `hash_b` and whose
/// next opcode is `second`: a 10-bit pair hash, with bit 10 set for a slot opcode. Bits 0 to 6
/// index [`Matcher::filter`] and bits 7 to 9 pick the bit there.
const fn key(hash_a: u8, hash_b: u8, second: u8) -> usize {
    ((second ^ hash_a) & 0x7F) as usize
        | (((second >> 5 ^ hash_b) & 7) as usize) << 7
        | ((hash_b & HASH_SLOT != 0) as usize) << 10
}

// Distinct pairs have distinct keys, so a key names its pair's block.
const _: () = {
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut j = 0;
        while j < i {
            let (p, q) = (PATTERNS[i], PATTERNS[j]);
            if p.op(0) != q.op(0) || p.op(1) != q.op(1) {
                let (pa, pb) = first_hashes(p.op(0));
                let (qa, qb) = first_hashes(q.op(0));
                assert!(key(pa, pb, p.op(1)) != key(qa, qb, q.op(1)), "pair hashes collide");
            }
            j += 1;
        }
        i += 1;
    }
};

/// Info bit of a match that jumps to a constant.
const HAS_TARGET: u32 = 1 << 16;

/// The candidates of one (first, second) opcode pair, longest first, laid out to be compared with
/// an instruction's window all at once. Unused lanes never match.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
struct Block {
    mask: [[u64; LANES]; 3],
    value: [[u64; LANES]; 3],
    /// Per lane, then for no match: span, slot << 8, and [`HAS_TARGET`].
    info: [u32; LANES + 1],
}

impl Block {
    const NONE: Self = Self { mask: [[0; LANES]; 3], value: [[1; LANES]; 3], info: [0; LANES + 1] };

    /// Returns the info of the first lane that matches window `w`, or 0.
    #[inline(always)]
    fn find(&self, w: &[u64; 3]) -> u32 {
        self.info[(self.hits(w) | 1 << LANES).trailing_zeros() as usize]
    }

    /// Returns the lanes that match window `w`, one bit each.
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
    #[inline(always)]
    fn hits(&self, w: &[u64; 3]) -> u32 {
        use core::arch::x86_64::*;
        // SAFETY: The target has AVX-512F, and `Block` is 64-byte aligned with each row 64 bytes.
        unsafe {
            let (m, v) =
                (self.mask.as_ptr().cast::<__m512i>(), self.value.as_ptr().cast::<__m512i>());
            // (w & mask) ^ value per qword, then the OR of the three.
            let d = |i: usize| {
                _mm512_ternarylogic_epi64::<0x6A>(
                    _mm512_set1_epi64(w[i] as i64),
                    _mm512_load_si512(m.add(i)),
                    _mm512_load_si512(v.add(i)),
                )
            };
            let d = _mm512_ternarylogic_epi64::<0xFE>(d(0), d(1), d(2));
            _mm512_testn_epi64_mask(d, d) as u32
        }
    }

    /// Returns the lanes that match window `w`, one bit each.
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2", not(target_feature = "avx512f")))]
    #[inline(always)]
    fn hits(&self, w: &[u64; 3]) -> u32 {
        use core::arch::x86_64::*;
        // SAFETY: The target has AVX2, and `Block` rows are 32-byte aligned halves.
        unsafe {
            let (m, v) =
                (self.mask.as_ptr().cast::<__m256i>(), self.value.as_ptr().cast::<__m256i>());
            let half = |h: usize| {
                let mut d = _mm256_setzero_si256();
                for i in 0..3 {
                    let x = _mm256_and_si256(
                        _mm256_set1_epi64x(w[i] as i64),
                        _mm256_load_si256(m.add(2 * i + h)),
                    );
                    d = _mm256_or_si256(
                        d,
                        _mm256_xor_si256(x, _mm256_load_si256(v.add(2 * i + h))),
                    );
                }
                _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(
                    d,
                    _mm256_setzero_si256(),
                ))) as u32
            };
            half(0) | half(1) << 4
        }
    }

    /// Returns the lanes that match window `w`, one bit each.
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    #[inline(always)]
    fn hits(&self, w: &[u64; 3]) -> u32 {
        let mut hits = 0;
        for i in 0..LANES {
            let d = ((w[0] & self.mask[0][i]) ^ self.value[0][i])
                | ((w[1] & self.mask[1][i]) ^ self.value[1][i])
                | ((w[2] & self.mask[2][i]) ^ self.value[2][i]);
            hits |= ((d == 0) as u32) << i;
        }
        hits
    }
}

/// The superinstructions of a configuration, prepared for [`translate`].
#[derive(Clone, Debug)]
pub struct Matcher {
    ops: Ops,
    /// By opcode: the group of a first opcode of an active pattern, [`FILL_GROUP`] for a slot in
    /// use, else 0.
    group: [u8; 256],
    /// Block by group and second opcode, 0 for none.
    pairs: [[u8; 256]; GROUPS + 1],
    blocks: [Block; BLOCKS],
    /// By slot: the offset of the constant jump target's PUSH immediate, and its size << 8.
    targets: [u16; 256],
    /// Filter on (first, second) opcode pairs for [`simd`]: by opcode, a 7-bit hash and, in
    /// `hash_b`, a 3-bit hash with [`IS_FIRST`] or [`HASH_SLOT`]; `filter` holds a bit per hash.
    hash_a: [u8; 256],
    hash_b: [u8; 256],
    filter: [u8; 128],
    /// Block by [`key`]: a slot opcode's keys name [`FILL_BLOCK`], other keys their pair's block
    /// or 0.
    keys: [u8; 2048],
    filler: u8,
    active: bool,
    variant: Variant,
}

/// Which superinstructions a translation uses: the patterns that apply, and the byte written over
/// slot opcodes in the code. Configurations with the same variant translate code the same way, so
/// a translation made for one applies to the other (most forks change opcodes no pattern uses).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Variant {
    /// Bit `i` is set when [`PATTERNS`]`[i]` applies.
    pub active: u128,
    /// The undefined opcode written over slot opcodes in the code.
    pub filler: u8,
}

const _: () = assert!(PATTERNS.len() <= 128, "a variant has a bit per pattern");

/// Index in [`PATTERNS`] by slot, or `u8::MAX`.
const SLOT_INDEX: [u8; 256] = {
    let mut index = [u8::MAX; 256];
    let mut i = 0;
    while i < PATTERNS.len() {
        index[PATTERNS[i].slot as usize] = i as u8;
        i += 1;
    }
    index
};

impl Matcher {
    /// Prepares the superinstructions that apply with `ops`; see [`Ops::is_active`].
    pub const fn new(ops: Ops) -> Self {
        let mut m = Self {
            ops,
            group: [0; 256],
            pairs: [[0; 256]; GROUPS + 1],
            blocks: [Block::NONE; BLOCKS],
            targets: [0; 256],
            hash_a: [0; 256],
            hash_b: [0; 256],
            filter: [0; 128],
            keys: [0; 2048],
            filler: op::INVALID,
            active: false,
            variant: Variant { active: 0, filler: 0 },
        };
        let mut slots = OpSet::EMPTY;
        let mut i = 0;
        while i < PATTERNS.len() {
            if ops.is_active(PATTERNS[i]) {
                slots.insert(PATTERNS[i].slot);
                m.active = true;
                m.variant.active |= 1 << i;
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
        // A slot opcode in the code becomes the filler: a one-byte match that always applies.
        let mut fill =
            Block { mask: [[0; LANES]; 3], value: [[0; LANES]; 3], info: [0; LANES + 1] };
        fill.info[0] = 1 | (m.filler as u32) << 8;
        m.blocks[FILL_BLOCK] = fill;
        let mut second = 0;
        while second < 256 {
            m.pairs[FILL_GROUP][second] = FILL_BLOCK as u8;
            second += 1;
        }
        let mut op = 0;
        while op < 256 {
            if slots.contains(op as u8) {
                m.group[op] = FILL_GROUP as u8;
            }
            op += 1;
        }
        // The pair filter: a first opcode's group spreads its pairs over the filter.
        let mut op = 0;
        while op < 256 {
            if slots.contains(op as u8) {
                m.hash_b[op] = HASH_SLOT;
            }
            op += 1;
        }
        let mut i = 0;
        while i < PATTERNS.len() {
            let sx = PATTERNS[i];
            if ops.is_active(sx) {
                let (first, second) = (sx.op(0) as usize, sx.op(1));
                (m.hash_a[first], m.hash_b[first]) = first_hashes(first as u8);
                let k = key(m.hash_a[first], m.hash_b[first], second);
                m.filter[k & 0x7F] |= 1 << (k >> 7);
            }
            i += 1;
        }
        // One block per (first, second) pair, its lanes longest first.
        let mut n = 1;
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
                    let mut block = Block::NONE;
                    let mut lane = 0;
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
                                assert!(lane < LANES, "more patterns share two opcodes than lanes");
                                let w = Window::of(sx);
                                let mut q = 0;
                                while q < 3 {
                                    block.mask[q][lane] = w.mask[q];
                                    block.value[q][lane] = w.value[q];
                                    q += 1;
                                }
                                let jump = if w.target_len != 0 { HAS_TARGET } else { 0 };
                                block.info[lane] = w.span as u32 | (w.slot as u32) << 8 | jump;
                                m.targets[w.slot as usize] =
                                    w.target_at as u16 | (w.target_len as u16) << 8;
                                lane += 1;
                            }
                            j += 1;
                        }
                        len -= 1;
                    }
                    m.blocks[n] = block;
                    m.group[first as usize] = g as u8 + 1;
                    m.pairs[g + 1][second as usize] = n as u8;
                    n += 1;
                }
                i += 1;
            }
            g += 1;
        }
        let mut k = 1024;
        while k < 2048 {
            m.keys[k] = FILL_BLOCK as u8;
            k += 1;
        }
        let mut i = 0;
        while i < PATTERNS.len() {
            let sx = PATTERNS[i];
            if ops.is_active(sx) {
                let g = m.group[sx.op(0) as usize] as usize;
                let k = key(m.hash_a[sx.op(0) as usize], m.hash_b[sx.op(0) as usize], sx.op(1));
                m.keys[k] = m.pairs[g][sx.op(1) as usize];
            }
            i += 1;
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

    /// Returns the variant of the translations it makes.
    #[inline]
    pub const fn variant(&self) -> Variant {
        let mut v = self.variant;
        v.filler = self.filler;
        v
    }
}

/// Stored translations: a record of the bytes translation changed, which turns the original code
/// back into its translation much faster than translating it again.
///
/// A record is a version byte, the [`Variant`] (16-byte pattern mask and filler), the code length
/// (u32), the number of changes (u32) and a checksum (u64) of all of it, then each change as the
/// distance from the previous one (LEB128) and its byte. All integers are little endian.
pub mod record {
    use super::{PATTERNS, SLOT_INDEX, Variant, is_slot, match_at, offsets, valid_target};
    use alloc::{boxed::Box, vec::Vec};

    const VERSION: u8 = 1;
    const HEADER: usize = 1 + 16 + 1 + 4 + 4 + 8;

    /// Returns the record of translation `translated` of `code`, which is `len` bytes long.
    pub fn encode(variant: Variant, code: &[u8], len: usize, translated: &[u8]) -> Vec<u8> {
        assert!(len <= code.len() && len <= translated.len() && len <= u32::MAX as usize);
        let mut out = Vec::with_capacity(HEADER + len / 4);
        out.push(VERSION);
        out.extend_from_slice(&variant.active.to_le_bytes());
        out.push(variant.filler);
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out.extend_from_slice(&[0; 12]);
        let (mut count, mut last) = (0u32, 0usize);
        let mut emit = |out: &mut Vec<u8>, pc: usize| {
            let mut delta = pc - last;
            while delta >= 0x80 {
                out.push(delta as u8 | 0x80);
                delta >>= 7;
            }
            out.push(delta as u8);
            out.push(translated[pc]);
            last = pc;
            count += 1;
        };
        // Changes are sparse: compare eight bytes at a time and visit only the bytes that differ.
        let mut pc = 0;
        while pc + 8 <= len {
            let word = |s: &[u8]| u64::from_le_bytes(s[pc..pc + 8].try_into().unwrap());
            let mut diff = word(code) ^ word(translated);
            while diff != 0 {
                let byte = diff.trailing_zeros() as usize / 8;
                emit(&mut out, pc + byte);
                diff &= !(0xFF << (8 * byte));
            }
            pc += 8;
        }
        for pc in pc..len {
            if translated[pc] != code[pc] {
                emit(&mut out, pc);
            }
        }
        out[22..26].copy_from_slice(&count.to_le_bytes());
        let sum = checksum(&out);
        out[26..34].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// Applies `record` to `code`, the padded code that is `len` bytes long with jump destinations
    /// `jumpdests`, and returns the variant and the translation.
    ///
    /// Returns `None` for a record that does not check out: of another version or length, with a
    /// wrong checksum, or with a change that is not at an instruction start past the last match,
    /// that is no superinstruction of its variant whose sequence is at that position, or whose
    /// constant jump target is no jump destination. Every translation accepted this way runs
    /// like the code: immediates are untouched and fused sequences do not overlap.
    pub fn decode(
        record: &[u8],
        code: &[u8],
        len: usize,
        jumpdests: &[u8],
    ) -> Option<(Variant, Box<[u8]>)> {
        if record.len() < HEADER || record[0] != VERSION || len > code.len() {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(record[i..i + 4].try_into().unwrap()) as usize;
        let active = u128::from_le_bytes(record[1..17].try_into().unwrap());
        let variant = Variant { active, filler: record[17] };
        let count = u32_at(22);
        let sum = u64::from_le_bytes(record[26..34].try_into().unwrap());
        if u32_at(18) != len || checksum(record) != sum {
            return None;
        }
        let mut out: Box<[u8]> = code.into();
        let (mut at, mut pc) = (HEADER, 0usize);
        // The next instruction start not inside an accepted match.
        let mut walk = 0usize;
        for i in 0..count {
            let mut delta = 0usize;
            let mut shift = 0;
            loop {
                let b = *record.get(at)?;
                at += 1;
                delta |= ((b & 0x7F) as usize).checked_shl(shift)?;
                shift += 7;
                if b < 0x80 {
                    break;
                }
            }
            if i > 0 && delta == 0 {
                return None;
            }
            pc = pc.checked_add(delta)?;
            let byte = *record.get(at)?;
            at += 1;
            if pc >= len {
                return None;
            }
            while walk < pc {
                walk += super::instruction_len(code[walk]);
            }
            if walk != pc {
                return None;
            }
            let slot_index = SLOT_INDEX[byte as usize] as usize;
            let end = if byte == variant.filler {
                (is_slot(code[pc]) && active >> SLOT_INDEX[code[pc] as usize] & 1 != 0)
                    .then_some(pc + 1)
            } else if slot_index < PATTERNS.len() && active >> slot_index & 1 != 0 {
                let sx = PATTERNS[slot_index];
                match_at(code, len, pc, sx).filter(|_| {
                    sx.static_jump().is_none_or(|n| {
                        let imm = pc + offsets(sx)[sx.len() - 2] + 1;
                        valid_target(&code[imm..imm + n], len, jumpdests)
                    })
                })
            } else {
                None
            };
            walk = end?;
            out[pc] = byte;
        }
        (at == record.len()).then_some((variant, out))
    }

    /// A 64-bit checksum of `record` with its checksum field as zero.
    fn checksum(record: &[u8]) -> u64 {
        let mut h = 0x243F_6A88_85A3_08D3u64;
        let mut mix = |w: u64| {
            h = (h ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            h ^= h >> 29;
        };
        let (head, tail) = record.split_at(HEADER);
        for c in head[..26].chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            mix(u64::from_le_bytes(w));
        }
        let mut chunks = tail.chunks_exact(8);
        for c in &mut chunks {
            mix(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let mut w = [0u8; 8];
        w[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
        mix(u64::from_le_bytes(w) ^ (record.len() as u64) << 56);
        h
    }
}

/// Translates legacy bytecode to the superinstructions of `matcher`.
///
/// `code` is the padded bytecode, `len` its length before padding, and `jumpdests` its jump
/// destinations, one bit per byte of `code[..len]`, least significant bit first. Returns a copy of
/// `code` in which the first byte of each sequence that matches an active pattern holds the
/// pattern's slot, and every other instruction whose opcode is a slot in use holds an undefined
/// opcode that is not a slot in use. Matching is greedy and longest first; a match that jumps to a
/// constant that is no jump destination keeps its original bytes.
///
/// Translation runs in two passes without data-dependent branches. The first walks the
/// instructions and lists those whose first two opcodes start a candidate; the second compares each
/// listed instruction with its candidates at once and takes a match when it starts at or after the
/// end of the last one.
pub fn translate(code: &[u8], len: usize, matcher: &Matcher, jumpdests: &[u8]) -> Box<[u8]> {
    use core::hint::select_unpredictable as select;
    assert!(len <= code.len());
    assert!(jumpdests.len() >= len.div_ceil(8));
    let mut out: Box<[u8]> = code.into();
    // Instructions below `fast` have the lookahead in bounds; candidates are pc << 11 | key.
    let fast = if len < 1 << 21 { len.min(code.len().saturating_sub(LOOKAHEAD)) } else { 0 };
    // Candidates, then the positions of matches that jump to a constant.
    let mut list: Vec<u32> = Vec::with_capacity(fast + 64 + WALKS + 1);
    let (src, dst, buf) = (code.as_ptr(), out.as_mut_ptr(), list.as_mut_ptr());
    let (keys, blocks) = (matcher.keys.as_ptr(), matcher.blocks.as_ptr());
    let mut pc;
    // SAFETY: `pc < fast` keeps every read in `code` and every write in `out`, the tables are
    // indexed by bytes and by blocks they hold, and each pass writes at most one entry per
    // instruction, below the capacity of `list`.
    let (jumps, mut free) = unsafe {
        let n;
        (n, pc) = candidates(src, code.len(), fast, matcher, buf);
        // The end of the last match taken.
        let mut free = 0;
        let mut j = 0;
        for i in 0..n {
            let c = *buf.add(i) as usize;
            let (at, block) = (c >> 11, *keys.add(c & 0x7FF) as usize);
            let p = src.add(at).cast::<u64>();
            let w = [
                u64::from_le(p.read_unaligned()),
                u64::from_le(p.add(1).read_unaligned()),
                u64::from_le(p.add(2).read_unaligned()),
            ];
            let info = (*blocks.add(block)).find(&w);
            let taken = (at >= free) & (info != 0);
            free = select(taken, at + (info & 0xFF) as usize, free);
            *dst.add(at) = select(taken, (info >> 8) as u8, *src.add(at));
            // `j <= i`, so this overwrites only candidates already read.
            *buf.add(j) = at as u32;
            j += (taken & (info & HAS_TARGET != 0)) as usize;
        }
        list.set_len(j);
        (list, free)
    };
    // A match that jumps to a constant that is no jump destination keeps its original bytes.
    for &at in &jumps {
        let at = at as usize;
        let t = matcher.targets[out[at] as usize] as usize;
        let (imm, n) = (t & 0xFF, t >> 8);
        if !valid_target(&code[at + imm..at + imm + n], len, jumpdests) {
            out[at] = code[at];
        }
    }
    while pc < len {
        let op = code[pc];
        if matcher.group[op as usize] as usize == FILL_GROUP {
            out[pc] = matcher.filler;
        } else if pc >= free
            && let Some(sx) = find_slow(code, len, pc, matcher)
        {
            let end = match_at(code, len, pc, sx).unwrap();
            let valid = sx.static_jump().is_none_or(|n| {
                let at = pc + offsets(sx)[sx.len() - 2] + 1;
                valid_target(&code[at..at + n], len, jumpdests)
            });
            if valid {
                out[pc] = sx.slot;
            }
            free = end;
        }
        pc += instruction_len(op);
    }
    out
}

/// One instruction of a candidate walk: lists it in `buf` at `*n` if its first two opcodes start a
/// candidate, and returns the next instruction.
///
/// # Safety
///
/// `src` holds [`LOOKAHEAD`] bytes past `pc`, `pc < 1 << 21`, and `buf` has room at `*n`.
#[inline(always)]
unsafe fn step(src: *const u8, pc: usize, m: &Matcher, buf: *mut u32, n: &mut usize) -> usize {
    // SAFETY: As the caller promises.
    unsafe {
        let op = *src.add(pc);
        let ilen = instruction_len(op);
        let (a, b) = (*m.hash_a.get_unchecked(op as usize), *m.hash_b.get_unchecked(op as usize));
        let k = key(a, b, *src.add(pc + ilen));
        *buf.add(*n) = (pc << 11 | k) as u32;
        *n += ((*m.keys.get_unchecked(k) != 0) & (b & (IS_FIRST | HASH_SLOT) != 0)) as usize;
        pc + ilen
    }
}

/// Walks the instructions from `pc` while below `end` and lists candidates in `buf`; returns how
/// many, with the first instruction at or after `end`.
///
/// # Safety
///
/// As for [`step`], with room in `buf` for an entry per byte from `pc` to `end`, plus one.
#[inline(always)]
unsafe fn walk(
    src: *const u8,
    mut pc: usize,
    end: usize,
    m: &Matcher,
    buf: *mut u32,
) -> (usize, usize) {
    let mut n = 0;
    while pc < end {
        // SAFETY: As the caller promises.
        pc = unsafe { step(src, pc, m, buf, &mut n) };
    }
    (n, pc)
}

/// Independent walks the scalar [`candidates`] runs at once.
const WALKS: usize = 4;

/// Lists the instructions below `fast` whose first two opcodes start a candidate in `buf` and
/// returns how many, with the first instruction at or after `fast`. Some lists may hold
/// instructions whose two opcodes start no candidate; matching skips them.
///
/// With AVX-512 VBMI2, whole 64-byte blocks run in [`simd`]. Otherwise, a walk from one instruction
/// to the next waits on each opcode load, so [`WALKS`] walks run interleaved, each from the start
/// of an equal part of the code as if an instruction started there. Instruction boundaries
/// resynchronize within a few instructions: a walk is kept from where the previous walk ended, if
/// it passed that point, and is redone from there otherwise.
///
/// # Safety
///
/// `src` holds `code_len` bytes, `fast + LOOKAHEAD <= code_len`, `fast < 1 << 21`, and `buf` has
/// room for an entry per byte below `fast`, plus 64 and [`WALKS`].
#[inline(always)]
unsafe fn candidates(
    src: *const u8,
    code_len: usize,
    fast: usize,
    matcher: &Matcher,
    buf: *mut u32,
) -> (usize, usize) {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512bw",
        target_feature = "avx512vbmi",
        target_feature = "avx512vbmi2"
    ))]
    // SAFETY: As the caller promises.
    unsafe {
        let (n, pc) = simd::candidates(src, code_len, fast, matcher, buf);
        let (more, pc) = walk(src, pc, fast, matcher, buf.add(n));
        return (n + more, pc);
    }
    #[allow(unreachable_code)]
    let _ = code_len;
    // SAFETY: Walk `k` writes at most one entry per byte of its part, from `bounds[k] + k`.
    unsafe {
        if fast < 256 * WALKS {
            return walk(src, 0, fast, matcher, buf);
        }
        let part = fast / WALKS;
        let bounds: [usize; WALKS + 1] =
            core::array::from_fn(|k| if k == WALKS { fast } else { k * part });
        let mut pc: [usize; WALKS] = core::array::from_fn(|k| bounds[k]);
        let mut n = [0; WALKS];
        while (0..WALKS).all(|k| pc[k] < bounds[k + 1]) {
            for k in 0..WALKS {
                pc[k] = step(src, pc[k], matcher, buf.add(bounds[k] + k), &mut n[k]);
            }
        }
        for k in 0..WALKS {
            let (more, end) =
                walk(src, pc[k], bounds[k + 1], matcher, buf.add(bounds[k] + k + n[k]));
            n[k] += more;
            pc[k] = end;
        }
        // Keep each later walk from where the previous one ended.
        let mut total = n[0];
        for k in 1..WALKS {
            let from = pc[k - 1];
            let at = bounds[k] + k;
            let mut q = bounds[k];
            while q < from {
                q += instruction_len(*src.add(q));
            }
            let (keep, count) = if q == from {
                // In step: skip the candidates before `from`.
                let mut skip = 0;
                while skip < n[k] && (*buf.add(at + skip) as usize >> 11) < from {
                    skip += 1;
                }
                (at + skip, n[k] - skip)
            } else {
                let (count, end) = walk(src, from, bounds[k + 1], matcher, buf.add(at));
                pc[k] = end;
                (at, count)
            };
            core::ptr::copy(buf.add(keep), buf.add(total), count);
            total += count;
        }
        (total, pc[WALKS - 1])
    }
}

/// Candidates of whole 64-byte blocks with AVX-512: instruction starts from jump destination
/// analysis's successor maps, and a vector filter on each start's first two opcodes.
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512bw",
    target_feature = "avx512vbmi",
    target_feature = "avx512vbmi2"
))]
mod simd {
    use super::{HASH_SLOT, IS_FIRST, Matcher};
    use core::arch::x86_64::*;

    /// Every opcode up to this, and `0x80..` as signed bytes, is one byte long.
    const PUSHX: i8 = 0x5F;

    const fn lanes(start: u8) -> [u8; 64] {
        let mut lanes = [0; 64];
        let mut i = 0;
        while i < 64 {
            lanes[i] = start.wrapping_add(i as u8);
            i += 1;
        }
        lanes
    }

    // SAFETY: Both are 64 bytes.
    const LANES: __m512i = unsafe { core::mem::transmute(lanes(0)) };
    /// Added to `max(opcode, PUSHX)` to get the lane of the next instruction.
    const END_OFFSETS: __m512i = unsafe { core::mem::transmute(lanes(0u8.wrapping_sub(0x5E))) };
    /// Second table of `vpermt2b`, which keeps offsets past the block.
    const EXITS: __m512i = unsafe { core::mem::transmute(lanes(64)) };

    /// Returns the successor maps of `ops` for 1, 2, 4, ..., 64 instructions.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    fn successors(ops: __m512i) -> [__m512i; 7] {
        let mut n =
            [_mm512_add_epi8(_mm512_max_epi8(ops, _mm512_set1_epi8(PUSHX)), END_OFFSETS); 7];
        for i in 1..7 {
            n[i] = _mm512_permutex2var_epi8(n[i - 1], n[i - 1], EXITS);
        }
        n
    }

    /// Moves each lane's position along the successor map if the result does not pass the lane.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    fn descend(successors: __m512i, positions: __m512i) -> __m512i {
        let next = _mm512_permutexvar_epi8(positions, successors);
        _mm512_mask_mov_epi8(positions, _mm512_cmple_epu8_mask(next, LANES), next)
    }

    /// Returns the lanes of the block with successor maps `n` that start an instruction.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    fn starts(n: &[__m512i; 7], entry: __m512i) -> u64 {
        let mut position = entry;
        for i in (1..6).rev() {
            position = descend(n[i], position);
        }
        _mm512_cmpeq_epi8_mask(position, LANES)
            | _mm512_cmpeq_epi8_mask(_mm512_permutexvar_epi8(position, n[0]), LANES)
    }

    /// Looks up each byte of `ops` in a 256-byte table.
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    fn lookup(table: &[__m512i; 4], ops: __m512i, high: __mmask64) -> __m512i {
        _mm512_mask_blend_epi8(
            high,
            _mm512_permutex2var_epi8(table[0], ops, table[1]),
            _mm512_permutex2var_epi8(table[2], ops, table[3]),
        )
    }

    /// Lists the candidates of whole 64-byte blocks below `fast` and returns how many, with the
    /// first instruction after the last block.
    ///
    /// # Safety
    ///
    /// As for [`super::candidates`].
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vbmi,avx512vbmi2")]
    pub(super) unsafe fn candidates(
        src: *const u8,
        code_len: usize,
        fast: usize,
        m: &Matcher,
        buf: *mut u32,
    ) -> (usize, usize) {
        // SAFETY: Tables are 64-byte multiples; loads stay below `code_len` (the next block's
        // load is masked to it); a block writes 64 entries at `buf + n`, with `n` at most the
        // instructions before it.
        unsafe {
            let load4 = |t: &[u8; 256]| -> [__m512i; 4] {
                core::array::from_fn(|i| _mm512_loadu_si512(t.as_ptr().add(64 * i).cast()))
            };
            let (hash_a, hash_b) = (load4(&m.hash_a), load4(&m.hash_b));
            let filter = [
                _mm512_loadu_si512(m.filter.as_ptr().cast()),
                _mm512_loadu_si512(m.filter.as_ptr().add(64).cast()),
            ];
            // 1 << k at byte k of each 8.
            let pow2 = _mm512_set1_epi64(0x8040_2010_0804_0201_u64 as i64);
            let mut entry = _mm512_setzero_si512();
            let (mut n, mut base) = (0, 0);
            while base + 64 <= fast {
                let ops = _mm512_loadu_si512(src.add(base).cast());
                let rest = (code_len - base - 64).min(64);
                let next =
                    _mm512_maskz_loadu_epi8(u64::MAX >> (64 - rest), src.add(base + 64).cast());
                let succ = successors(ops);
                let starts = starts(&succ, entry);
                entry =
                    _mm512_sub_epi8(_mm512_permutexvar_epi8(entry, succ[6]), _mm512_set1_epi8(64));
                // The opcode of each lane's next instruction.
                let second = _mm512_permutex2var_epi8(ops, succ[0], next);
                let high = _mm512_movepi8_mask(ops);
                let a = lookup(&hash_a, ops, high);
                let b = lookup(&hash_b, ops, high);
                let byte =
                    _mm512_permutex2var_epi8(filter[0], _mm512_xor_si512(second, a), filter[1]);
                let bit = _mm512_and_si512(
                    _mm512_xor_si512(_mm512_srli_epi16(second, 5), b),
                    _mm512_set1_epi8(7),
                );
                let hit = _mm512_test_epi8_mask(byte, _mm512_shuffle_epi8(pow2, bit));
                let first = _mm512_test_epi8_mask(b, _mm512_set1_epi8(IS_FIRST as i8));
                let slot = _mm512_test_epi8_mask(b, _mm512_set1_epi8(HASH_SLOT as i8));
                let cand = starts & (first & hit | slot);
                // Each candidate as pc << 11 | key, from its lane, the key's bits 7 to 10 and its
                // bits 0 to 6.
                let lanes = _mm512_maskz_compress_epi8(cand, LANES);
                let high = _mm512_maskz_compress_epi8(
                    cand,
                    _mm512_or_si512(
                        bit,
                        _mm512_and_si512(_mm512_srli_epi16(b, 1), _mm512_set1_epi8(8)),
                    ),
                );
                let low = _mm512_maskz_compress_epi8(
                    cand,
                    _mm512_and_si512(_mm512_xor_si512(second, a), _mm512_set1_epi8(0x7F)),
                );
                let at = _mm512_set1_epi32((base << 11) as i32);
                macro_rules! store {
                    ($($q:literal)*) => {$(
                        let p = _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<$q>(lanes));
                        let h = _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<$q>(high));
                        let l = _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<$q>(low));
                        let c = _mm512_ternarylogic_epi32::<0xFE>(_mm512_slli_epi32(p, 11), _mm512_slli_epi32(h, 7), l);
                        _mm512_storeu_si512(buf.add(n + 16 * $q).cast(), _mm512_add_epi32(c, at));
                    )*};
                }
                store!(0 1 2 3);
                n += cand.count_ones() as usize;
                base += 64;
            }
            (n, base + _mm512_cvtsi512_si32(entry) as u8 as usize)
        }
    }
}

/// Returns whether the big-endian immediate `bytes` is a jump destination in `marks`.
fn valid_target(bytes: &[u8], len: usize, marks: &[u8]) -> bool {
    let n = bytes.len();
    if n > 8 && bytes[..n - 8].iter().any(|&b| b != 0) {
        return false;
    }
    let target = bytes[n.saturating_sub(8)..].iter().fold(0u64, |acc, &b| acc << 8 | b as u64);
    target < len as u64 && marks[target as usize / 8] >> (target % 8) & 1 != 0
}

/// Returns the code offset of each opcode of `sx` from its first, and the sequence length last.
pub const fn offsets(sx: Sx) -> [usize; 9] {
    let mut offsets = [0; 9];
    let mut i = 0;
    while i < sx.len() {
        offsets[i + 1] = offsets[i] + instruction_len(sx.op(i));
        i += 1;
    }
    let mut j = sx.len() + 1;
    while j < 9 {
        offsets[j] = offsets[sx.len()];
        j += 1;
    }
    offsets
}

/// [`Matcher::find`] near the end of code without padding.
#[cold]
fn find_slow(code: &[u8], len: usize, pc: usize, matcher: &Matcher) -> Option<Sx> {
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
    longest
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

    /// All Prague opcodes, nothing hooked; slots free.
    fn prague() -> Ops {
        let mut ops = Ops::default();
        for op in 0..=255u8 {
            let defined = matches!(op, 0x00..=0x0B | 0x10..=0x1D | 0x20 | 0x30..=0x4A | 0x50..=0x9F | 0xA0..=0xA4 | 0xF0..=0xF5 | 0xFA | 0xFD..=0xFF);
            if defined { ops.fusable.insert(op) } else { ops.free.insert(op) }
        }
        ops
    }

    /// [`translate`] with the jump destinations of `code[..len]`.
    fn tr(code: &[u8], len: usize, matcher: &Matcher) -> Box<[u8]> {
        translate(code, len, matcher, &reference_jumpdests(&code[..len]))
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

    /// The slot of the pattern that runs `ops`.
    fn slot(ops: &[u8]) -> u8 {
        PATTERNS.iter().find(|p| p.ops == pack(ops)).expect("no such pattern").slot
    }

    fn pad(code: &[u8]) -> Vec<u8> {
        let mut v = code.to_vec();
        v.extend_from_slice(&[0; 33]);
        v
    }

    #[test]
    fn slots() {
        for (i, sx) in PATTERNS.iter().enumerate() {
            assert_eq!(sx.slot, SLOTS[i]);
            assert!(prague().free.contains(sx.slot));
            assert!(Window::of(*sx).span as usize <= WINDOW);
        }
        assert!(Matcher::new(prague()).is_active());
    }

    #[test]
    fn fuses_longest_and_keeps_layout() {
        use op::*;
        // DUP1 PUSH4 sel EQ PUSH2 dst JUMPI, then PUSH2 dst JUMPI, to the JUMPDEST at 15.
        let code =
            [DUP1, PUSH4, 1, 2, 3, 4, EQ, PUSH2, 0, 15, JUMPI, PUSH2, 0, 15, JUMPI, JUMPDEST];
        let padded = pad(&code);
        let out = tr(&padded, code.len(), &Matcher::new(prague()));
        assert_eq!(out.len(), padded.len());
        assert_eq!(out[0], PATTERNS[1].slot);
        assert_eq!(&out[1..11], &code[1..11]);
        assert_eq!(out[11], PATTERNS[0].slot);
        assert_eq!(&out[12..], &padded[12..]);
        // A constant target that is no JUMPDEST leaves the sequence unfused.
        let mut code = code;
        code[13] = 14;
        let padded = pad(&code);
        let out = tr(&padded, code.len(), &Matcher::new(prague()));
        assert_eq!(out[0], PATTERNS[1].slot);
        assert_eq!(&out[11..], &padded[11..]);
    }

    #[test]
    fn does_not_fuse_disabled_opcodes() {
        use op::*;
        let code = [JUMPDEST, PUSH0, PUSH1, 1, PUSH1, 2, SHL, SUB];
        let padded = pad(&code);
        let mut ops = prague();
        let out = tr(&padded, code.len(), &Matcher::new(ops));
        assert_eq!((out[0], out[2]), (slot(&[JUMPDEST, PUSH0]), slot(&[PUSH1, PUSH1, SHL, SUB])));
        // Before Shanghai PUSH0 is undefined, and before Constantinople SHL is: only the sequence
        // of enabled opcodes fuses.
        ops.fusable = OpSet::EMPTY;
        for op in [JUMPDEST, PUSH1, SUB] {
            ops.fusable.insert(op);
        }
        let out = tr(&padded, code.len(), &Matcher::new(ops));
        assert_eq!((out[0], out[1], out[2]), (JUMPDEST, PUSH0, slot(&[PUSH1, PUSH1])));
        assert_eq!(&out[3..], &padded[3..]);
    }

    #[test]
    fn slot_bytes_in_code_become_invalid() {
        use op::*;
        let slot = PATTERNS[0].slot;
        // A slot as an instruction, and as PUSH data, which is not an instruction.
        let code = [slot, PUSH1, slot, PUSH2, 0, 7, JUMPI, JUMPDEST];
        let padded = pad(&code);
        let out = tr(&padded, code.len(), &Matcher::new(prague()));
        // 0x0C, the first undefined opcode, halts like the slot would without superinstructions.
        assert_eq!(out[0], 0x0C);
        assert_eq!(out[2], slot);
        assert_eq!(out[3], PATTERNS[0].slot);
        // A slot of a pattern that is not active stays as it is.
        let mut ops = prague();
        ops.fusable = OpSet::EMPTY;
        assert_eq!(tr(&padded, code.len(), &Matcher::new(ops))[0], slot);
    }

    #[test]
    fn nothing_fuses_with_eip8024() {
        use op::*;
        let code = [PUSH2, 0, 5, JUMPI, PUSH1, 1, ADD];
        let padded = pad(&code);
        let mut ops = prague();
        ops.fusable.insert(DUPN);
        ops.free = OpSet::EMPTY;
        for op in SLOTS {
            ops.free.insert(op);
        }
        assert!(PATTERNS.iter().all(|sx| !ops.is_active(*sx)));
        assert_eq!(&tr(&padded, code.len(), &Matcher::new(ops))[..], &padded[..]);
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
        let out = tr(&padded, code.len(), &Matcher::new(prague()));
        assert_eq!(&out[..], &padded[..]);
    }

    /// Greedy longest match, one candidate at a time.
    fn reference(code: &[u8], len: usize, ops: &Ops) -> Vec<u8> {
        let m = Matcher::new(*ops);
        let marks = reference_jumpdests(&code[..len]);
        let mut out = code.to_vec();
        let mut pc = 0;
        while pc < len {
            if m.group[code[pc] as usize] as usize == FILL_GROUP {
                out[pc] = m.filler;
                pc += 1;
                continue;
            }
            match find_slow(code, len, pc, &m) {
                Some(sx) => {
                    let ok = sx.static_jump().is_none_or(|n| {
                        let at = pc + offsets(sx)[sx.len() - 2] + 1;
                        valid_target(&code[at..at + n], len, &marks)
                    });
                    if ok {
                        out[pc] = sx.slot;
                    }
                    pc = match_at(code, len, pc, sx).unwrap();
                }
                None => pc += instruction_len(code[pc]),
            }
        }
        out
    }

    #[test]
    fn records_round_trip_and_reject_damage() {
        let mut s = 0x6A09E667F3BCC908u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let alphabet: Vec<u8> =
            PATTERNS.iter().flat_map(|p| (0..p.len()).map(move |i| p.op(i))).collect();
        let m = Matcher::new(prague());
        let mut changed = 0;
        for round in 0..2000 {
            let n = (next() % 600) as usize;
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
            let padded = pad(&code);
            let jumps = reference_jumpdests(&code);
            let out = translate(&padded, n, &m, &jumps);
            let rec = record::encode(m.variant(), &padded, n, &out);
            let (variant, back) = record::decode(&rec, &padded, n, &jumps).expect("round trip");
            assert_eq!(variant, m.variant());
            assert_eq!(&back[..], &out[..], "round {round}");
            // Any one flipped byte is rejected.
            if rec.len() > 34 {
                let mut bad = rec.clone();
                let i = 34 + (next() as usize % (rec.len() - 34));
                bad[i] ^= 1 << (next() % 8);
                assert!(record::decode(&bad, &padded, n, &jumps).is_none());
                changed += 1;
            }
            // A record for other code of the same length is rejected unless the changes fit it.
            assert!(record::decode(&rec, &padded, n + 1, &jumps).is_none());
        }
        assert!(changed > 1000);
    }

    #[test]
    fn records_that_change_immediates_or_overlap_are_rejected() {
        use op::*;
        let m = Matcher::new(prague());
        let check = |code: &[u8], at: usize, ops: &[u8]| {
            let padded = pad(code);
            let jumps = reference_jumpdests(code);
            let mut forged = translate(&padded, code.len(), &m, &jumps);
            forged[at] = slot(ops);
            let rec = record::encode(m.variant(), &padded, code.len(), &forged);
            record::decode(&rec, &padded, code.len(), &jumps).is_none()
        };
        // POP POP as the immediate of PUSH2: not an instruction start.
        assert!(check(&[PUSH2, POP, POP, 0x00], 1, &[POP, POP]));
        // POP POP inside DUP2 POP POP ... after a match that covers it.
        assert!(check(&[POP, POP, POP, POP, 0x00], 1, &[POP, POP]));
    }

    #[test]
    fn records_list_every_changed_byte() {
        // Changes at every distance from word boundaries and gaps that need two LEB128 bytes.
        let len = 1000;
        let code: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        let mut translated = code.clone();
        let changed = [0, 1, 7, 8, 9, 15, 16, 200, 999];
        for &pc in &changed {
            translated[pc] ^= 0x5A;
        }
        let rec = record::encode(Variant::default(), &code, len, &translated);
        let mut expected = Vec::new();
        let mut last = 0;
        for &pc in &changed {
            let mut delta = pc - last;
            while delta >= 0x80 {
                expected.push(delta as u8 | 0x80);
                delta >>= 7;
            }
            expected.extend([delta as u8, translated[pc]]);
            last = pc;
        }
        assert_eq!(&rec[34..], &expected[..]);
        assert_eq!(u32::from_le_bytes(rec[22..26].try_into().unwrap()), changed.len() as u32);
    }

    #[test]
    fn record_with_a_bad_jump_is_rejected() {
        use op::*;
        // PUSH2 0x0005 JUMPI, JUMPDEST at 5: fused. A record that fuses a jump to 6 is not
        // accepted, even with a correct checksum.
        let code = [PUSH2, 0, 5, JUMPI, 0x00, JUMPDEST, PUSH2, 0, 6, JUMPI];
        let padded = pad(&code);
        let jumps = reference_jumpdests(&code);
        let m = Matcher::new(prague());
        let out = translate(&padded, code.len(), &m, &jumps);
        assert_eq!(out[0], slot(&[PUSH2, JUMPI]));
        assert_eq!(out[6], PUSH2);
        let mut forged = out.clone();
        forged[6] = slot(&[PUSH2, JUMPI]);
        let rec = record::encode(m.variant(), &padded, code.len(), &forged);
        assert!(record::decode(&rec, &padded, code.len(), &jumps).is_none());
    }

    #[test]
    fn long_code_matches_reference() {
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
        let mut codes: Vec<Vec<u8>> = Vec::new();
        // Walks that never resynchronize: every byte a PUSH32, or PUSH2 with JUMPDEST data.
        codes.push(alloc::vec![op::PUSH32; 5000]);
        codes.push([op::PUSH2, op::JUMPDEST, op::PUSH2].repeat(2000));
        codes.push([op::PUSH1, op::PUSH1].repeat(3001));
        for _ in 0..300 {
            let n = 1000 + (next() % 9000) as usize;
            codes.push(
                (0..n)
                    .map(|_| {
                        let r = next();
                        if r % 5 == 0 {
                            (r >> 24) as u8
                        } else {
                            alphabet[(r >> 8) as usize % alphabet.len()]
                        }
                    })
                    .collect(),
            );
        }
        for code in codes {
            let padded = pad(&code);
            let got = tr(&padded, code.len(), &Matcher::new(ops));
            assert_eq!(&got[..], &reference(&padded, code.len(), &ops)[..], "len {}", code.len());
        }
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
                let got = tr(&padded, len, &Matcher::new(ops));
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
            let out = tr(&padded, n, &Matcher::new(ops));
            assert_eq!(&out[n..], &padded[n..]);
            // Walk the instructions: every changed byte starts a full match of its pattern, or is a
            // slot turned INVALID; nothing changes elsewhere.
            let mut pc = 0;
            while pc < n {
                let (a, b) = (padded[pc], out[pc]);
                if a == b {
                    assert!(!is_slot(b), "slot left in code");
                    pc += instruction_len(a);
                } else if b == 0x0C {
                    assert!(is_slot(a));
                    pc += 1;
                } else {
                    let sx = *PATTERNS.iter().find(|p| p.slot == b).expect("slot");
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
