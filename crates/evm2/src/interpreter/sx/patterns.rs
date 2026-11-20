//! The superinstructions: the opcode sequences that fuse, and the opcodes they run under.

use crate::interpreter::{op, opcode::OpCode};

/// A superinstruction: 2 to 8 opcodes run under one undefined opcode, its slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sx {
    /// The opcode translated code runs it under.
    pub(crate) slot: u8,
    /// The opcodes it runs, the first in the low byte.
    pub(crate) ops: u64,
}

impl Sx {
    /// Returns the number of opcodes it runs.
    #[inline]
    pub(crate) const fn len(self) -> usize {
        8 - self.ops.leading_zeros() as usize / 8
    }

    /// Returns its `i`th opcode.
    #[inline]
    pub(crate) const fn op(self, i: usize) -> u8 {
        (self.ops >> (8 * i)) as u8
    }

    /// Returns the code offset of each of its opcodes from the first, then the length of the
    /// sequence in the code.
    pub(crate) const fn offsets(self) -> [usize; 9] {
        let mut offsets = [0; 9];
        let mut i = 0;
        while i < 8 {
            let len = if i < self.len() { instruction_len(self.op(i)) } else { 0 };
            offsets[i + 1] = offsets[i] + len;
            i += 1;
        }
        offsets
    }

    /// Returns `(input, growth)`: the stack must hold `input` words for the sequence to run, and
    /// it grows at most `growth` words above its starting height while it runs.
    pub(crate) const fn stack_bounds(self) -> (usize, usize) {
        let (mut height, mut min, mut max) = (0isize, 0isize, 0isize);
        let mut i = 0;
        while i < self.len() {
            let info = OpCode::new_or_unknown(self.op(i));
            let (input, output) = (info.inputs() as usize, info.outputs() as usize);
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

    /// Returns the size of the PUSH that supplies the target of a final JUMP or JUMPI, when that
    /// PUSH comes right before it: the jump goes to a constant, which translation checks.
    pub(crate) const fn static_jump(self) -> Option<usize> {
        let n = self.len();
        match (self.op(n - 1), self.op(n - 2)) {
            (op::JUMP | op::JUMPI, push @ op::PUSH1..=op::PUSH32) => {
                Some((push - op::PUSH0) as usize)
            }
            _ => None,
        }
    }
}

/// Returns the length of the instruction `op` starts, immediate included, with PUSH as the only
/// instruction with an immediate.
#[inline(always)]
pub(crate) const fn instruction_len(op: u8) -> usize {
    match op {
        op::PUSH1..=op::PUSH32 => (op - op::PUSH1) as usize + 2,
        _ => 1,
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
        packed |= (op as u64) << (8 * i);
        i += 1;
    }
    packed
}

/// Opcodes the patterns run under, in order, all undefined from Frontier through Osaka. A pattern
/// applies only where its slot is undefined, so a later spec that defines one turns it off. 0x0C is
/// left out: translation writes it over slot bytes in the code.
const SLOTS: [u8; 105] = {
    let ranges = [
        (0xB0, 0xEF),
        (0x21, 0x2F),
        (0xA5, 0xAF),
        (0x0D, 0x0F),
        (0x1F, 0x1F),
        (0x4B, 0x4F),
        (0xF6, 0xF9),
        (0xFB, 0xFC),
    ];
    let mut slots = [0; 105];
    let (mut n, mut r) = (0, 0);
    while r < ranges.len() {
        let (mut op, end): (u8, u8) = ranges[r];
        while op <= end {
            slots[n] = op;
            n += 1;
            op += 1;
        }
        r += 1;
    }
    assert!(n == slots.len());
    slots
};

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
pub(crate) const PATTERNS: [Sx; 105] = patterns![
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

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The superinstruction that runs `ops`.
    pub(crate) fn sx(ops: &[u8]) -> Sx {
        *PATTERNS.iter().find(|p| p.ops == pack(ops)).expect("no such pattern")
    }

    #[test]
    fn patterns_are_distinct() {
        for (i, p) in PATTERNS.iter().enumerate() {
            assert!(PATTERNS[..i].iter().all(|q| q.ops != p.ops), "duplicate pattern {i}");
        }
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
}
