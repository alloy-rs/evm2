//! Translated programs and the semantics shared by translation and execution.

use alloy_primitives::U256;
use core::cmp::Ordering;
use evm2::interpreter::{
    Word,
    i256::{i256_cmp, i256_div, i256_mod},
    op,
};

/// Block ID of a jump to an invalid destination.
pub(crate) const INVALID_BLOCK: u32 = u32::MAX;

/// Section ID that marks "no section starts here".
pub(crate) const NO_SECTION: u32 = u32::MAX;

/// Moves start of an exit without moves.
pub(crate) const NO_MOVES: u32 = u32::MAX;

/// Compare kind that tests a single operand against zero, as a fused `JUMPI` without a compare.
pub(crate) const CMP_NZ: u8 = op::ISZERO;

/// An operand: a stack slot relative to the stack top at block entry, or an immediate.
///
/// Both are byte offsets, so handlers address them without arithmetic: a slot's from the entry
/// top, an immediate's into the program's constants, plus one. Words are 32 bytes, so bit 0 is
/// free to mark immediates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Operand(pub(crate) u32);

impl Operand {
    /// Creates a slot operand.
    pub(crate) const fn slot(offset: i32) -> Self {
        Self((offset * 32) as u32)
    }

    /// Creates an immediate operand from a constant index.
    pub(crate) const fn imm(index: u32) -> Self {
        Self(index * 32 + 1)
    }

    /// Returns whether this is an immediate.
    #[inline(always)]
    pub(crate) const fn is_imm(raw: u32) -> bool {
        raw & 1 != 0
    }

    /// Returns the byte offset of an immediate into the constants.
    #[inline(always)]
    pub(crate) const fn imm_bytes(raw: u32) -> usize {
        (raw - 1) as usize
    }

    /// Returns the byte offset of a slot from the entry top.
    #[inline(always)]
    pub(crate) const fn slot_bytes(raw: u32) -> isize {
        raw as i32 as isize
    }
}

/// What a record does. Each kind and operand mode links to its own handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `d = op(a, b)`.
    Alu2 { op: u8, neg: u8, a_imm: bool, b_imm: bool },
    /// `d = op(a)`, with a slot operand.
    Alu1 { op: u8, neg: u8 },
    /// `d = mload(a)`.
    Mload { a_imm: bool },
    /// `MSTORE` or `MSTORE8` of value `b` at offset `a`.
    Mstore { op: u8, a_imm: bool, b_imm: bool },
    /// `d = op()` for a frame or message field: [`is_env`].
    Env { op: u8 },
    /// `d = calldataload(a)`.
    CallDataLoad { a_imm: bool },
    /// Static jump to block `c`.
    Jump { moves: u8 },
    /// Dynamic jump to the destination in `a`.
    JumpDyn,
    /// Fused compare and branch to block `c`. Not taken, it falls through to block `g`, or with
    /// `cont` continues at the next record after charging section `g` (if any).
    Br { cmp: u8, neg: u8, cont: bool, a_imm: bool, b_imm: bool },
    /// Branch on `a` to the dynamic destination in `b`, falling through to block `g`.
    BrDyn,
    /// Fall through into block `c`.
    Fall { moves: u8 },
    /// The host's instruction at pc `a` on the canonical stack.
    Generic,
    /// Stop with [`Halt`] `a`.
    Halt,
    /// Hand the rest of the frame to the interpreter at pc `a`.
    Bail,
}

/// How a [`Kind::Halt`] record stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Halt {
    /// Falling off the end of the code.
    Stop = 0,
    /// A static jump to an invalid destination.
    InvalidJump = 1,
    /// The designated invalid opcode (`0xFE`).
    InvalidFE = 2,
}

/// One translated record.
///
/// Field meaning depends on [`Kind`]. Exit records keep the start of their moves in `d`
/// ([`NO_MOVES`] for none), and the stack height they leave, relative to the block's entry top,
/// in `e`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inst {
    pub(crate) kind: Kind,
    pub(crate) a: u32,
    pub(crate) b: u32,
    pub(crate) c: u32,
    pub(crate) d: u32,
    pub(crate) e: u32,
    pub(crate) g: u32,
}

impl Inst {
    pub(crate) const fn new(kind: Kind) -> Self {
        Self { kind, a: 0, b: 0, c: 0, d: 0, e: 0, g: 0 }
    }
}

/// One entry of an exit's moves.
///
/// The first entry is a header whose `dst` counts the moves after it. Each move copies the
/// [`Operand`] `src` to the slot at byte offset `dst` from the entry top; moves run in order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Move {
    pub(crate) dst: i32,
    pub(crate) src: u32,
}

/// Exits with up to this many moves get a handler with the copies unrolled.
pub(crate) const MAX_UNROLLED_MOVES: u8 = 8;

/// A gas and stack section: charged and checked once, before its first instruction.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Section {
    /// Static gas of every instruction in the section.
    pub(crate) gas: u64,
    /// Stack height the section needs, relative to its start.
    pub(crate) inputs: i32,
    /// Maximum stack growth, relative to its start.
    pub(crate) max_growth: i32,
    /// Stack height at the section's start, relative to the block's entry top.
    pub(crate) base: i32,
    /// Slots above the entry top the block writes, including temporaries. Only set on a block's
    /// first section; a frame whose stack cannot fit them hands the block to the interpreter.
    pub(crate) need: i32,
}

/// A translated basic block.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Block {
    /// Index of its first record.
    pub(crate) first: u32,
    /// Its first pc.
    pub(crate) pc: u32,
    /// Static gas of its entry section.
    pub(crate) gas: u64,
    /// Lowest entry stack length that needs no checks beyond gas.
    pub(crate) min_len: u32,
    /// Entry stack lengths up to `min_len + span` need no checks beyond gas: the entry section
    /// neither underflows nor overflows, and the block's slots and temporaries fit.
    pub(crate) span: u32,
    /// Its entry section, for the precise checks of other lengths.
    pub(crate) section: u32,
}

/// A contract translated into records, independent of the host type.
#[derive(Clone, Debug, Default)]
pub(crate) struct Program {
    pub(crate) insts: Vec<Inst>,
    pub(crate) consts: Vec<U256>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) sections: Vec<Section>,
    pub(crate) moves: Vec<Move>,
    /// Block of each pc that is a valid jump destination, or [`INVALID_BLOCK`].
    pub(crate) jump_map: Vec<u32>,
}

/// Returns whether [`eval2`] implements `op`.
pub(crate) const fn is_alu2(op: u8) -> bool {
    matches!(
        op,
        op::ADD
            | op::MUL
            | op::SUB
            | op::DIV
            | op::SDIV
            | op::MOD
            | op::SMOD
            | op::SIGNEXTEND
            | op::LT
            | op::GT
            | op::SLT
            | op::SGT
            | op::EQ
            | op::AND
            | op::OR
            | op::XOR
            | op::BYTE
            | op::SHL
            | op::SHR
            | op::SAR
    )
}

/// Returns whether `op` reads a frame or message field that [`Kind::Env`] implements.
pub(crate) const fn is_env(op: u8) -> bool {
    matches!(op, op::ADDRESS | op::CALLER | op::CALLVALUE | op::CALLDATASIZE | op::RETURNDATASIZE)
}

/// Returns whether a value of `op` can become a branch condition or flip under `ISZERO`.
pub(crate) const fn is_cond(op: u8) -> bool {
    matches!(op, op::EQ | op::LT | op::GT | op::SLT | op::SGT | op::AND)
}

/// Returns the `neg` that `ISZERO` of a condition with `neg` leaves.
///
/// Compares are booleans, so negation flips them. `AND` starts as a plain word (0), becomes
/// `iszero(a & b)` (1), and then alternates with `a & b != 0` (2).
pub(crate) const fn flip(op: u8, neg: u8) -> u8 {
    match (op, neg) {
        (op::AND, 0) | (op::AND, 2) => 1,
        (op::AND, _) => 2,
        (_, n) => n ^ 1,
    }
}

/// Evaluates a binary operation as the interpreter does, with `a` the top of the stack.
#[inline(always)]
pub(crate) fn eval2(op: u8, neg: u8, a: &Word, b: &Word) -> Word {
    match op {
        op::ADD => a.wrapping_add(*b),
        op::MUL => a.wrapping_mul(*b),
        op::SUB => a.wrapping_sub(*b),
        op::DIV => {
            if b.is_zero() {
                Word::ZERO
            } else {
                a.wrapping_div(*b)
            }
        }
        op::SDIV => i256_div(*a, *b),
        op::MOD => {
            if b.is_zero() {
                Word::ZERO
            } else {
                a.wrapping_rem(*b)
            }
        }
        op::SMOD => i256_mod(*a, *b),
        op::SIGNEXTEND => {
            if *a < Word::from(31) {
                let bit_index = (8 * a.as_limbs()[0] + 7) as usize;
                let mask = (Word::ONE << bit_index) - Word::ONE;
                if b.bit(bit_index) { *b | !mask } else { *b & mask }
            } else {
                *b
            }
        }
        op::LT | op::GT | op::SLT | op::SGT | op::EQ => Word::from(cond(op, neg, a, b) as u64),
        op::AND => match neg {
            0 => *a & *b,
            _ => Word::from(cond(op, neg, a, b) as u64),
        },
        op::OR => *a | *b,
        op::XOR => *a ^ *b,
        op::BYTE => {
            let index = to_usize_saturated(a);
            if index < 32 { Word::from(b.byte(31 - index)) } else { Word::ZERO }
        }
        op::SHL => {
            let shift = to_usize_saturated(a);
            if shift < 256 { *b << shift } else { Word::ZERO }
        }
        op::SHR => {
            let shift = to_usize_saturated(a);
            if shift < 256 { *b >> shift } else { Word::ZERO }
        }
        op::SAR => {
            let shift = to_usize_saturated(a);
            if shift < 256 {
                b.arithmetic_shr(shift)
            } else if b.bit(255) {
                Word::MAX
            } else {
                Word::ZERO
            }
        }
        _ => unreachable!(),
    }
}

/// Evaluates a branch condition: compare `cmp` of `a` and `b`, negated by `neg`.
#[inline(always)]
pub(crate) fn cond(cmp: u8, neg: u8, a: &Word, b: &Word) -> bool {
    let c = match cmp {
        op::EQ => a == b,
        op::LT => a < b,
        op::GT => a > b,
        op::SLT => i256_cmp(a, b) == Ordering::Less,
        op::SGT => i256_cmp(a, b) == Ordering::Greater,
        op::AND => !(*a & *b).is_zero(),
        CMP_NZ => !a.is_zero(),
        _ => unreachable!(),
    };
    c ^ (neg == 1)
}

/// Evaluates a unary operation: `ISZERO` (`neg` 1 tests non-zero instead) or `NOT`.
#[inline(always)]
pub(crate) fn eval1(op: u8, neg: u8, a: &Word) -> Word {
    match op {
        op::ISZERO => Word::from((a.is_zero() ^ (neg == 1)) as u64),
        op::NOT => !*a,
        _ => unreachable!(),
    }
}

#[inline(always)]
const fn to_usize_saturated(word: &Word) -> usize {
    let limbs = word.as_limbs();
    if (limbs[1] | limbs[2] | limbs[3]) != 0 { usize::MAX } else { limbs[0] as usize }
}
