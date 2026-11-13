//! Superinstructions: runs of opcodes that the interpreter executes in one dispatch.
//!
//! A frame without an inspector runs on a *fused* copy of its bytecode ([`fuse`]), once the code
//! has run often enough to pay for the copy ([`BYTES_PER_RUN`]). The copy has the same length and
//! the same bytes as the code, except that the first opcode of each run that matches a
//! [`Pattern`] is replaced with the pattern's id, a byte that no fork assigns to an opcode. The
//! handler at that id runs the instructions of the whole run and dispatches once at its end.
//!
//! Only the first byte of a run changes, so every other position still holds its own opcode: a
//! jump into the middle of a run executes the remaining instructions one by one, and immediates
//! are read from the fused code unchanged. A handler that cannot run its instructions the way the
//! plain handlers would, because the remaining gas does not cover them, runs the plain handler of
//! the first instruction instead, and the dispatch continues with the next original opcode.
//! Results, gas and halt reasons are therefore those of the plain interpreter.

use super::op::{self, *};
use crate::{EvmTypesHost, OpcodeConfig, interpreter::private::InstructionFlags};
use alloc::boxed::Box;

/// The most opcodes a [`Pattern`] holds.
pub(crate) const MAX_PATTERN_LEN: usize = 4;

/// First byte of the range reserved for the fused code. No fork defines an opcode in it, and a
/// config that does runs without superinstructions.
pub(crate) const RESERVED_FIRST: u8 = 0xa5;

/// Last byte of the reserved range.
pub(crate) const RESERVED_LAST: u8 = 0xdf;

/// Stands in the fused code for an original opcode in the reserved range. Its handler is the one
/// for an undefined opcode.
pub(crate) const ESCAPE: u8 = RESERVED_FIRST;

/// Bytes of code that one run pays for. The interpreter fuses a
/// [`Bytecode`](crate::bytecode::Bytecode) once its runs so far have paid for all of its bytes.
///
/// Fusing reads every instruction of the code, which costs about as much as the superinstructions
/// save in a few hundred runs of a large contract. Code that runs once or twice, like initcode,
/// therefore never pays for it, and code of up to this size is fused on its first run. This rate
/// and [`GAS_PER_BYTE`] are the ones that execute the transactions of
/// `data/mainnet-25347446-25347455.bin.zst` the fastest when no code is fused at the start: fusing
/// all code on its first run costs several times what the superinstructions then save there.
pub(crate) const BYTES_PER_RUN: usize = 128;

/// Gas that a run uses to pay for one more byte of code, on top of [`BYTES_PER_RUN`], so that a
/// long run is worth many short ones.
pub(crate) const GAS_PER_BYTE: u64 = 512;

/// A run of opcodes executed as one superinstruction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pattern {
    ops: [u8; MAX_PATTERN_LEN],
    len: u8,
}

impl Pattern {
    const fn new(ops: &[u8]) -> Self {
        assert!(ops.len() >= 2 && ops.len() <= MAX_PATTERN_LEN);
        let mut out = [op::STOP; MAX_PATTERN_LEN];
        let mut i = 0;
        while i < ops.len() {
            assert!(is_fusable_opcode(ops[i]), "opcode cannot be part of a superinstruction");
            // A jump ends the run.
            assert!(i + 1 == ops.len() || !matches!(ops[i], JUMP | JUMPI));
            out[i] = ops[i];
            i += 1;
        }
        Self { ops: out, len: ops.len() as u8 }
    }

    /// Returns the number of opcodes in the run.
    #[inline]
    pub(crate) const fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns the opcode at `index`, or `STOP` past the end of the run.
    #[inline]
    pub(crate) const fn op(&self, index: usize) -> u8 {
        self.ops[index]
    }
}

macro_rules! patterns {
    ($([$($op:ident),+ $(,)?],)*) => {
        [$(Pattern::new(&[$($op),+])),*]
    };
}

/// The superinstructions, most frequent first.
///
/// Chosen greedily by the dispatches each one saves in the transactions of
/// `data/mainnet-25347446-25347455.bin.zst`. Together they save 36% of them.
pub(crate) const PATTERNS: [Pattern; 58] = patterns![
    [PUSH2, JUMPI],
    [PUSH2, JUMP],
    [ISZERO, PUSH2, JUMPI],
    [JUMPDEST, PUSH1],
    [PUSH4, EQ, PUSH2, JUMPI],
    [POP, POP],
    [PUSH1, PUSH1, SHL, SUB],
    [SWAP1, POP],
    [MSTORE, PUSH1],
    [SWAP1, JUMP],
    [JUMPDEST, POP],
    [LT, ISZERO, PUSH2, JUMPI],
    [SWAP2, POP, POP, JUMP],
    [JUMPDEST, PUSH2],
    [DUP3, ADD],
    [PUSH1, ADD],
    [SWAP2, SWAP1, POP, JUMP],
    [PUSH2, DUP3, PUSH2, JUMP],
    [DUP2, MSTORE, PUSH1, ADD],
    [SWAP2, SWAP1],
    [DUP2, ADD],
    [JUMPDEST, SWAP2, POP],
    [JUMPDEST, PUSH0],
    [DUP2, EQ, PUSH2, JUMPI],
    [POP, POP, POP, JUMP],
    [PUSH20, AND],
    [JUMPDEST, PUSH1, DUP3, DUP3],
    [LT, PUSH2, JUMPI],
    [JUMPDEST, DUP1],
    [GT, PUSH2, JUMPI],
    [JUMPDEST, DUP3],
    [PUSH1, SHR],
    [JUMPDEST, DUP4, DUP2],
    [DUP4, ADD],
    [JUMPDEST, SWAP3],
    [DUP3, SUB, PUSH2, JUMPI],
    [SWAP1, DUP4, PUSH2, JUMP],
    [PUSH1, MLOAD],
    [GT, ISZERO, PUSH2, JUMPI],
    [JUMPDEST, SWAP1],
    [DUP5, ADD],
    [PUSH2, DUP2, PUSH2, JUMP],
    [PUSH1, NOT],
    [PUSH1, DUP3, ADD],
    [DUP1, SWAP2, SUB, SWAP1],
    [SWAP1, DUP2],
    [MLOAD, PUSH1],
    [PUSH1, MSTORE],
    [ADD, PUSH2, JUMP],
    [PUSH1, SHL],
    [DUP2, AND],
    [JUMPDEST, DUP2],
    [JUMPDEST, SWAP1, POP],
    [DUP3, AND],
    [PUSH1, DUP1],
    [PUSH1, ADD, SWAP1, JUMP],
    [SWAP1, DUP2, MSTORE, PUSH1],
    [ISZERO, ISZERO],
];

/// Calls `$m` with the index of every [`PATTERNS`] entry, as literals for const generic
/// arguments.
macro_rules! for_each_pattern_index {
    ($m:ident) => {
        $m! { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, }
    };
}
pub(crate) use for_each_pattern_index;

const _: () = {
    assert!(ESCAPE as usize + PATTERNS.len() <= RESERVED_LAST as usize);
    macro_rules! count {
        ($($i:literal,)*) => { [$($i),*].len() };
    }
    assert!(for_each_pattern_index!(count) == PATTERNS.len());
    // Every opcode in the reserved range is undefined, so that the escape's handler reports what
    // the original opcode would.
    let mut opcode = RESERVED_FIRST;
    while opcode <= RESERVED_LAST {
        assert!(super::opcode::OPCODE_INFO[opcode as usize].is_none());
        opcode += 1;
    }
    // No two patterns are the same run.
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut j = i + 1;
        while j < PATTERNS.len() {
            let (a, b) = (&PATTERNS[i], &PATTERNS[j]);
            let same = a.len == b.len
                && a.ops[0] == b.ops[0]
                && a.ops[1] == b.ops[1]
                && a.ops[2] == b.ops[2]
                && a.ops[3] == b.ops[3];
            assert!(!same, "duplicate superinstruction");
            j += 1;
        }
        i += 1;
    }
};

/// Returns the byte that stands for `PATTERNS[index]` in the fused code.
#[inline]
pub(crate) const fn pattern_id(index: usize) -> u8 {
    ESCAPE + 1 + index as u8
}

/// Returns whether `opcode` may be part of a pattern: an instruction of one fixed length that
/// touches only the stack, memory, the program counter and the frame's inputs.
const fn is_fusable_opcode(opcode: u8) -> bool {
    matches!(
        opcode,
        ADD | MUL
            | SUB
            | LT
            | GT
            | SLT
            | SGT
            | EQ
            | ISZERO
            | AND
            | OR
            | XOR
            | NOT
            | BYTE
            | SHL
            | SHR
            | SAR
            | CALLER
            | CALLVALUE
            | CALLDATALOAD
            | CALLDATASIZE
            | POP
            | MLOAD
            | MSTORE
            | JUMP
            | JUMPI
            | JUMPDEST
            | PUSH0..=PUSH32
            | DUP1..=DUP16
            | SWAP1..=SWAP16
    )
}

/// Returns whether a table for `config` may hold superinstructions.
///
/// It may not when the config defines an opcode in the reserved range, or when
/// [`OpcodeConfig::set_superinstructions`] turned them off. A config that adds opcodes to `base`,
/// the base config of its spec, or overrides instructions of it has to turn them on itself:
/// [`fuse`] assumes that only `PUSH1..=PUSH32`, `DUPN`, `SWAPN` and `EXCHANGE` are followed by
/// immediate bytes, which an instruction of its own may not keep to.
pub(crate) const fn enabled<T: EvmTypesHost>(
    config: &OpcodeConfig<T>,
    base: &OpcodeConfig<T>,
) -> bool {
    if config.origin_spec() as u8 != base.origin_spec() as u8 {
        return false;
    }
    let mut own_instructions = false;
    let mut opcode = 0;
    loop {
        if config.is_defined(opcode) {
            if opcode >= RESERVED_FIRST && opcode <= RESERVED_LAST {
                return false;
            }
            own_instructions |= !base.is_defined(opcode) || config.is_overridden(base, opcode);
        }
        if opcode == u8::MAX {
            break;
        }
        opcode += 1;
    }
    // The fused code replaces the first opcode of each pattern with the pattern's id. A pattern
    // the config does not fuse runs that opcode's own instruction on the id, so an overridden or
    // hooked instruction there would read the id instead of its opcode.
    let mut index = 0;
    while index < PATTERNS.len() {
        let first = PATTERNS[index].op(0);
        if config.is_defined(first)
            && (config.is_overridden(base, first)
                || config.instruction(first).flags.contains(InstructionFlags::HOOKED))
        {
            return false;
        }
        index += 1;
    }
    match config.superinstructions() {
        Some(enabled) => enabled,
        None => !own_instructions,
    }
}

/// Returns whether `config` runs `pattern` as a superinstruction: it defines every instruction of
/// the run as `base` does, and none of them is hooked.
pub(crate) const fn fusable<T: EvmTypesHost>(
    config: &OpcodeConfig<T>,
    base: &OpcodeConfig<T>,
    pattern: &Pattern,
) -> bool {
    let mut i = 0;
    while i < pattern.len() {
        let opcode = pattern.ops[i];
        if !config.is_defined(opcode)
            || config.is_overridden(base, opcode)
            || config.instruction(opcode).flags.contains(InstructionFlags::HOOKED)
        {
            return false;
        }
        i += 1;
    }
    true
}

/// Returns the class of each opcode and the number of classes: one for each opcode in a
/// pattern, and class 0 for all the others.
const fn opcode_classes() -> ([u8; 256], usize) {
    let mut classes = [0; 256];
    let mut count = 1;
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut j = 0;
        while j < PATTERNS[i].len() {
            let opcode = PATTERNS[i].ops[j] as usize;
            if classes[opcode] == 0 {
                classes[opcode] = count as u8;
                count += 1;
            }
            j += 1;
        }
        i += 1;
    }
    (classes, count)
}

/// Returns whether patterns `a` and `b` start with the same `len` opcodes.
const fn same_prefix(a: &Pattern, b: &Pattern, len: usize) -> bool {
    if a.len() < len || b.len() < len {
        return false;
    }
    let mut i = 0;
    while i < len {
        if a.ops[i] != b.ops[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Returns the number of states of the [`Matcher`]: the distinct prefixes of the patterns, the
/// empty one included.
const fn matcher_states() -> usize {
    let mut states = 1;
    let mut i = 0;
    while i < PATTERNS.len() {
        let mut len = 1;
        while len <= PATTERNS[i].len() {
            let mut first = true;
            let mut j = 0;
            while j < i {
                first &= !same_prefix(&PATTERNS[i], &PATTERNS[j], len);
                j += 1;
            }
            states += first as usize;
            len += 1;
        }
        i += 1;
    }
    states
}

const CLASSES: usize = opcode_classes().1;
const STATES: usize = matcher_states();

/// Finds every pattern in a sequence of opcodes in one pass: an Aho-Corasick automaton whose
/// states are the prefixes of the patterns.
struct Matcher {
    /// Class of each opcode.
    classes: [u8; 256],
    /// The state after each state and class of opcode: the longest prefix of a pattern that the
    /// opcodes read so far end with.
    next: [[u8; CLASSES]; STATES],
    /// The patterns that the opcodes read so far end with at each state, by length: one more than
    /// the index of the pattern with `n + 2` opcodes, or 0 for none.
    ends: [[u8; MAX_PATTERN_LEN - 1]; STATES],
}

impl Matcher {
    const fn new() -> Self {
        assert!(STATES <= u8::MAX as usize + 1);
        let (classes, _) = opcode_classes();

        // The trie of the patterns. State 0 is its root.
        let mut children = [[0u8; CLASSES]; STATES];
        let mut paths = [[0u8; MAX_PATTERN_LEN]; STATES];
        let mut depths = [0usize; STATES];
        let mut states = 1;
        let mut i = 0;
        while i < PATTERNS.len() {
            let pattern = &PATTERNS[i];
            let mut state = 0;
            let mut j = 0;
            while j < pattern.len() {
                let class = classes[pattern.ops[j] as usize] as usize;
                if children[state][class] == 0 {
                    children[state][class] = states as u8;
                    paths[states] = paths[state];
                    paths[states][j] = pattern.ops[j];
                    depths[states] = j + 1;
                    states += 1;
                }
                state = children[state][class] as usize;
                j += 1;
            }
            i += 1;
        }
        assert!(states == STATES);

        let mut next = [[0u8; CLASSES]; STATES];
        let mut ends = [[0u8; MAX_PATTERN_LEN - 1]; STATES];
        // By depth, so that the shorter prefix a state falls back to is complete.
        let mut depth = 0;
        while depth <= MAX_PATTERN_LEN {
            let mut state = 0;
            while state < STATES {
                if depths[state] != depth {
                    state += 1;
                    continue;
                }
                let path = &paths[state];

                // The longest proper suffix of the path that is a prefix of a pattern.
                let mut fallback = 0;
                let mut start = 1;
                while start < depth {
                    let mut suffix = 0;
                    let mut i = start;
                    while i < depth {
                        suffix = children[suffix][classes[path[i] as usize] as usize] as usize;
                        if suffix == 0 {
                            break;
                        }
                        i += 1;
                    }
                    if suffix != 0 {
                        fallback = suffix;
                        break;
                    }
                    start += 1;
                }

                let mut class = 1;
                while class < CLASSES {
                    next[state][class] = if children[state][class] != 0 {
                        children[state][class]
                    } else if state == 0 {
                        0
                    } else {
                        next[fallback][class]
                    };
                    class += 1;
                }

                // The suffixes of the path that are patterns.
                let mut len = 2;
                while len <= depth {
                    let mut i = 0;
                    while i < PATTERNS.len() {
                        let pattern = &PATTERNS[i];
                        let mut same = pattern.len() == len;
                        let mut j = 0;
                        while same && j < len {
                            same = pattern.ops[j] == path[depth - len + j];
                            j += 1;
                        }
                        if same {
                            ends[state][len - 2] = i as u8 + 1;
                        }
                        i += 1;
                    }
                    len += 1;
                }
                state += 1;
            }
            depth += 1;
        }
        Self { classes, next, ends }
    }
}

const MATCHER: Matcher = Matcher::new();

/// Returns the length of the instruction `opcode` starts, as the jump destination analysis sees
/// it: the immediates of `DUPN`, `SWAPN` and `EXCHANGE` are scanned as opcodes.
#[inline(always)]
const fn scan_len(opcode: u8) -> usize {
    let push = opcode.wrapping_sub(PUSH1);
    if push < 32 { push as usize + 2 } else { 1 }
}

/// Returns the fused copy of `code`, legacy bytecode with its padding, whose first `original_len`
/// bytes are the original code.
///
/// The first opcode of each run that matches a pattern becomes the pattern's id, taking the
/// longest pattern that matches. An original opcode in the reserved range becomes [`ESCAPE`].
/// Every other byte is copied.
pub(crate) fn fuse(code: &[u8], original_len: usize) -> Box<[u8]> {
    const NO_ENDS: [u8; MAX_PATTERN_LEN - 1] = [0; MAX_PATTERN_LEN - 1];
    static M: Matcher = MATCHER;

    let mut fused = Box::<[u8]>::from(code);
    let code = &code[..original_len];
    // Where the last instructions start, indexed by their number modulo the length.
    let mut starts = [0; MAX_PATTERN_LEN];
    let mut count = 0;
    let mut state = 0;
    let mut pc = 0;
    // The byte after `DUPN`, `SWAPN` or `EXCHANGE` is their immediate on the forks that define
    // them. It is never a `PUSH` or a `JUMPDEST` when valid, so it cannot start a run, and it
    // stays as it is for the instruction to read. An immediate that is one of these three bytes
    // has no immediate of its own: the byte after it is the next opcode.
    let mut immediate = false;
    while let Some(&opcode) = code.get(pc) {
        let class = if immediate { 0 } else { M.classes[opcode as usize] };
        state = M.next[state as usize][class as usize];
        starts[count % MAX_PATTERN_LEN] = pc;
        let ends = &M.ends[state as usize];
        if *ends != NO_ENDS {
            // A longer pattern from the same start ends later and replaces this one.
            for (n, &end) in ends.iter().enumerate() {
                if end != 0 {
                    let start = starts[(count - (n + 1)) % MAX_PATTERN_LEN];
                    fused[start] = pattern_id(end as usize - 1);
                }
            }
        } else if !immediate && (RESERVED_FIRST..=RESERVED_LAST).contains(&opcode) {
            fused[pc] = ESCAPE;
        }
        immediate = !immediate && matches!(opcode, DUPN | SWAPN | EXCHANGE);
        count += 1;
        pc += scan_len(opcode);
    }
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfig, BaseEvmConfigSelector, EvmConfig, ExecutionConfig, SpecId,
        interpreter::{InstrStop, Word},
        test_utils::{RunConfig, TestInterpreter, TestTypes, run_with},
    };
    use alloc::vec::Vec;
    use evm2_macros::instruction;
    use rand::{RngExt, SeedableRng, rngs::StdRng};

    fn pad(code: &[u8]) -> Vec<u8> {
        let mut padded = code.to_vec();
        padded.extend([0; 33]);
        padded
    }

    fn id(ops: &[u8]) -> u8 {
        let index = PATTERNS
            .iter()
            .position(|pattern| &pattern.ops[..pattern.len()] == ops)
            .expect("not a pattern");
        pattern_id(index)
    }

    #[test]
    fn fuses_first_opcode_only() {
        let code = [PUSH2, 0x00, 0x05, JUMPI, STOP, JUMPDEST, PUSH1, 0x01, STOP];
        let padded = pad(&code);
        let fused = fuse(&padded, code.len());
        let mut expected = padded;
        expected[0] = id(&[PUSH2, JUMPI]);
        expected[5] = id(&[JUMPDEST, PUSH1]);
        assert_eq!(&*fused, &expected[..]);
    }

    #[test]
    fn takes_longest_pattern() {
        let code = [ISZERO, PUSH2, 0x00, 0x00, JUMPI];
        let fused = fuse(&pad(&code), code.len());
        assert_eq!(fused[0], id(&[ISZERO, PUSH2, JUMPI]));
        assert_eq!(fused[1], id(&[PUSH2, JUMPI]));
    }

    #[test]
    fn keeps_push_data() {
        // The data of `PUSH3` spells `PUSH2 _ _`, and `JUMPI` follows it.
        let code = [PUSH3, PUSH2, POP, POP, JUMPI, POP, POP];
        let padded = pad(&code);
        let fused = fuse(&padded, code.len());
        assert_eq!(fused[..5], code[..5]);
        assert_eq!(fused[5], id(&[POP, POP]));
    }

    #[test]
    fn escapes_reserved_opcodes() {
        let code = [RESERVED_LAST, PUSH1, RESERVED_LAST, RESERVED_FIRST + 1];
        let fused = fuse(&pad(&code), code.len());
        assert_eq!(fused[..4], [ESCAPE, PUSH1, RESERVED_LAST, ESCAPE]);
    }

    #[test]
    fn keeps_immediates() {
        for opcode in [DUPN, SWAPN, EXCHANGE] {
            // `POP POP` is a pattern, but the first `POP` is the immediate.
            let code = [opcode, POP, POP, POP, POP];
            let fused = fuse(&pad(&code), code.len());
            assert_eq!(fused[..3], [opcode, POP, id(&[POP, POP])]);
        }
    }

    #[test]
    fn immediate_has_no_immediate() {
        for opcode in [DUPN, SWAPN, EXCHANGE] {
            for immediate in [DUPN, SWAPN, EXCHANGE] {
                // The immediate spells an opcode with an immediate, and the byte after it is a
                // reserved opcode that runs next.
                let code = [opcode, immediate, RESERVED_LAST, POP, POP];
                let fused = fuse(&pad(&code), code.len());
                assert_eq!(fused[..4], [opcode, immediate, ESCAPE, id(&[POP, POP])]);
            }
        }
    }

    #[test]
    fn reserved_opcode_after_immediate_is_undefined() {
        // `EXCHANGE` with the immediate `0xe6` swaps two items, and `0xb0` after it is undefined.
        let mut code = [PUSH0; 12].to_vec();
        code.extend([EXCHANGE, DUPN, 0xb0, STOP]);
        let execution_config =
            ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::AMSTERDAM);
        for inspect in [false, true] {
            let config = RunConfig::new(code.clone()).spec(SpecId::AMSTERDAM);
            let interp = run_with(config, execution_config.clone(), inspect);
            assert_eq!(interp.err, InstrStop::OpcodeNotFound);
            assert_eq!(interp.stack().len(), 12);
        }
    }

    #[test]
    fn does_not_match_into_padding() {
        let code = [PUSH2, 0x00];
        let padded = pad(&code);
        assert_eq!(&*fuse(&padded, code.len()), &padded[..]);
        let code = [PUSH32];
        let padded = pad(&code);
        assert_eq!(&*fuse(&padded, code.len()), &padded[..]);
    }

    /// Opcodes of the patterns and their neighbours, and ones that end a frame or are undefined.
    const FILLER: &[u8] = &[
        ADD,
        SUB,
        MUL,
        LT,
        GT,
        EQ,
        ISZERO,
        AND,
        OR,
        NOT,
        SHL,
        SHR,
        POP,
        MLOAD,
        MSTORE,
        MSTORE8,
        CALLVALUE,
        CALLDATASIZE,
        CALLDATALOAD,
        JUMPDEST,
        JUMPDEST,
        PUSH0,
        PUSH1,
        PUSH1,
        PUSH2,
        PUSH4,
        PUSH20,
        PUSH32,
        DUP1,
        DUP2,
        DUP3,
        DUP4,
        DUP5,
        SWAP1,
        SWAP2,
        SWAP3,
        PC,
        GAS,
        MSIZE,
        STOP,
        RETURN,
        REVERT,
        INVALID,
        RESERVED_FIRST,
        RESERVED_LAST,
        0xa8,
        DUPN,
        0x0c,
    ];

    /// Immediates of `DUPN`, `SWAPN` and `EXCHANGE`: valid and invalid ones, the three opcodes
    /// themselves, reserved bytes and starts of patterns.
    const IMMEDIATES: &[u8] = &[
        0x00,
        0x01,
        0x10,
        0x5a,
        JUMPDEST,
        PUSH1,
        PUSH2,
        0x80,
        0x81,
        0x8f,
        POP,
        SWAP1,
        RESERVED_FIRST,
        0xb0,
        RESERVED_LAST,
        DUPN,
        SWAPN,
        EXCHANGE,
        0xff,
    ];

    /// Appends `opcode` with random immediate bytes. A two-byte immediate often is a destination.
    fn emit(code: &mut Vec<u8>, pushes: &mut Vec<usize>, opcode: u8, rng: &mut StdRng) {
        code.push(opcode);
        if (PUSH1..=PUSH32).contains(&opcode) {
            if opcode == PUSH2 {
                pushes.push(code.len());
            }
            let len = (opcode - PUSH1 + 1) as usize;
            // Mostly small values, so that they are usable memory offsets and shifts.
            let small = rng.random_ratio(3, 4);
            for i in 0..len {
                let byte = if small && i + 1 < len { 0 } else { rng.random::<u8>() };
                code.push(byte);
            }
        }
    }

    fn random_program(rng: &mut StdRng) -> Vec<u8> {
        let mut code = Vec::new();
        let mut pushes = Vec::new();
        // Operands for the first instructions, so that most programs run past them.
        for _ in 0..rng.random_range(0..24) {
            emit(&mut code, &mut pushes, PUSH1, rng);
        }
        for _ in 0..rng.random_range(1..40) {
            if rng.random_ratio(2, 3) {
                let pattern = &PATTERNS[rng.random_range(0..PATTERNS.len())];
                for &opcode in &pattern.ops[..pattern.len()] {
                    emit(&mut code, &mut pushes, opcode, rng);
                }
            } else if rng.random_ratio(1, 8) {
                // An instruction with an immediate, which may itself spell one, a reserved
                // opcode or the start of a pattern.
                code.push([DUPN, SWAPN, EXCHANGE][rng.random_range(0..3)]);
                code.push(IMMEDIATES[rng.random_range(0..IMMEDIATES.len())]);
                if rng.random_ratio(1, 2) {
                    code.push(rng.random_range(RESERVED_FIRST..=RESERVED_LAST));
                }
            } else {
                emit(&mut code, &mut pushes, FILLER[rng.random_range(0..FILLER.len())], rng);
            }
        }
        // Point most two-byte pushes at a jump destination.
        let destinations = (0..code.len()).filter(|&pc| code[pc] == JUMPDEST).collect::<Vec<_>>();
        if !destinations.is_empty() {
            for &at in &pushes {
                if rng.random_ratio(4, 5) {
                    let destination = destinations[rng.random_range(0..destinations.len())];
                    code[at..at + 2].copy_from_slice(&(destination as u16).to_be_bytes());
                }
            }
        }
        code
    }

    fn outcome(mut interp: TestInterpreter) -> impl PartialEq + core::fmt::Debug {
        let memory = interp.memory.len();
        (
            interp.err,
            interp.gas_remaining(),
            interp.gas_refunded(),
            interp.stack().to_vec(),
            interp.memory(0, memory).to_vec(),
            interp.output().to_vec(),
        )
    }

    /// Runs random programs with and without superinstructions and compares the frames, at every
    /// gas limit up to the gas a program uses, so that each of its instructions runs out of gas.
    fn assert_matches_plain_handlers(
        seed: u64,
        rounds: usize,
        mut execution: impl FnMut(usize) -> (SpecId, ExecutionConfig<TestTypes>),
    ) {
        const GAS_LIMIT: u64 = 100_000;
        let mut rng = StdRng::seed_from_u64(seed);
        for round in 0..rounds {
            let code = random_program(&mut rng);
            let (spec, execution_config) = execution(round);
            assert!(execution_config.fused_instructions.is_some());
            let run = |gas_limit, inspect| {
                let config = RunConfig::new(code.clone()).spec(spec).gas_limit(gas_limit);
                run_with(config, execution_config.clone(), inspect)
            };
            // A frame that ran out of gas has no meaningful remaining gas.
            let remaining = run(GAS_LIMIT, true).gas_remaining();
            let used = GAS_LIMIT.checked_sub(remaining).unwrap_or(GAS_LIMIT);
            for gas_limit in (0..=used.min(300)).chain([GAS_LIMIT]) {
                let fused = outcome(run(gas_limit, false));
                let plain = outcome(run(gas_limit, true));
                assert_eq!(fused, plain, "code={code:02x?} spec={spec:?} gas_limit={gas_limit}");
            }
        }
    }

    /// Every failure of an instruction inside a superinstruction, at every gas limit, leaves the
    /// frame as the plain handlers do.
    #[test]
    fn superinstructions_match_plain_handlers() {
        let specs = [SpecId::OSAKA, SpecId::AMSTERDAM, SpecId::LONDON, SpecId::FRONTIER];
        assert_matches_plain_handlers(0x5eed, 4_000, |round| {
            let spec = specs[round % specs.len()];
            (spec, ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(spec))
        });
    }

    /// Returns the fused code like [`fuse`], trying every pattern at every instruction.
    fn fuse_reference(code: &[u8], original_len: usize) -> Vec<u8> {
        let mut fused = code.to_vec();
        let mut pc = 0;
        let mut immediate = false;
        while pc < original_len {
            let opcode = code[pc];
            if !immediate {
                if (RESERVED_FIRST..=RESERVED_LAST).contains(&opcode) {
                    fused[pc] = ESCAPE;
                }
                let mut longest = 0;
                for (index, pattern) in PATTERNS.iter().enumerate() {
                    let mut at = pc;
                    let matches = pattern.ops[..pattern.len()].iter().all(|&opcode| {
                        let matches = at < original_len && code[at] == opcode;
                        at += scan_len(opcode);
                        matches
                    });
                    if matches && pattern.len() > longest {
                        longest = pattern.len();
                        fused[pc] = pattern_id(index);
                    }
                }
            }
            immediate = !immediate && matches!(opcode, DUPN | SWAPN | EXCHANGE);
            pc += scan_len(opcode);
        }
        fused
    }

    #[test]
    fn fuse_finds_every_longest_pattern() {
        let mut rng = StdRng::seed_from_u64(0xf05e);
        for round in 0..20_000 {
            let mut code = random_program(&mut rng);
            if round % 4 == 0 {
                // Opcodes of the patterns only, with whatever follows a push as its data.
                code.clear();
                for _ in 0..rng.random_range(0..200) {
                    let pattern = &PATTERNS[rng.random_range(0..PATTERNS.len())];
                    code.push(pattern.ops[rng.random_range(0..pattern.len())]);
                }
            }
            let padded = pad(&code);
            assert_eq!(
                &*fuse(&padded, code.len()),
                &fuse_reference(&padded, code.len())[..],
                "code={code:02x?}",
            );
            // Code that already ends with zeros has no padding.
            assert_eq!(&*fuse(&code, code.len()), &fuse_reference(&code, code.len())[..]);
        }
    }

    /// An opcode that no fork defines, outside the reserved range.
    const ADDED_OPCODE: u8 = 0x0c;

    #[instruction]
    fn add_one_more([a, b]: [Word]) -> out {
        *out = *a + *b + Word::from(1);
    }

    const fn osaka() -> OpcodeConfig<TestTypes> {
        OpcodeConfig::<TestTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>()
    }

    /// Declares a config of `TestTypes` that changes the Osaka opcodes with `$change`.
    macro_rules! test_config {
        ($name:ident, | $config:ident | $change:expr) => {
            struct $name;

            impl EvmConfig<TestTypes> for $name {
                const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
                const OPCODE_CONFIG: &'static OpcodeConfig<TestTypes> = &{
                    let mut $config = osaka();
                    $change;
                    $config
                };
            }
        };
    }

    test_config!(ReservedOpcode, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(RESERVED_LAST, 3)
    });
    test_config!(ReservedOpcodeOptIn, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(RESERVED_FIRST, 3);
        config.set_superinstructions(true)
    });
    test_config!(AddedOpcode, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(ADDED_OPCODE, 3)
    });
    test_config!(AddedOpcodeOptIn, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(ADDED_OPCODE, 3);
        config.set_superinstructions(true)
    });
    test_config!(OptOut, |config| config.set_superinstructions(false));
    test_config!(OverriddenAdd, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(ADD, 3)
    });
    test_config!(OverriddenAddOptIn, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(ADD, 3);
        config.set_superinstructions(true)
    });
    test_config!(RepricedAdd, |config| config.set_static_gas(ADD, 11));
    test_config!(OverriddenAndOptIn, |config| {
        config.set_instruction::<add_one_more<TestTypes>>(AND, 3);
        config.set_superinstructions(true)
    });

    fn fuses<C: EvmConfig<TestTypes>>() -> bool {
        ExecutionConfig::<TestTypes>::for_config::<C>().fused_instructions.is_some()
    }

    #[test]
    fn base_configs_run_superinstructions() {
        let mut spec = Some(SpecId::FRONTIER);
        while let Some(current) = spec {
            let config =
                ExecutionConfig::<TestTypes>::for_base_spec::<BaseEvmConfigSelector>(current);
            assert!(config.fused_instructions.is_some(), "{current:?}");
            spec = current.next();
        }
    }

    #[test]
    fn config_decides_superinstructions() {
        assert!(!fuses::<ReservedOpcode>());
        assert!(!fuses::<ReservedOpcodeOptIn>());
        assert!(!fuses::<AddedOpcode>());
        assert!(fuses::<AddedOpcodeOptIn>());
        assert!(!fuses::<OptOut>());
        assert!(!fuses::<OverriddenAdd>());
        // `ADD` starts a pattern, whose fused code would hide it from its instruction.
        assert!(PATTERNS.iter().any(|pattern| pattern.op(0) == ADD));
        assert!(!fuses::<OverriddenAddOptIn>());
        assert!(fuses::<OverriddenAndOptIn>());
        assert!(fuses::<RepricedAdd>());
    }

    #[test]
    fn overridden_instruction_is_not_fused() {
        let (base, config) = (&osaka(), OverriddenAndOptIn::OPCODE_CONFIG);
        for pattern in &PATTERNS {
            let has_and = pattern.ops[..pattern.len()].contains(&AND);
            assert_eq!(fusable(config, base, pattern), !has_and, "{pattern:?}");
        }

        // `DUP2 AND` is a pattern, and the config's `AND` adds its operands and one more.
        let code = [PUSH1, 2, PUSH1, 3, DUP2, AND, STOP];
        let fused = fuse(&pad(&code), code.len());
        assert_eq!(fused[4], id(&[DUP2, AND]));
        let execution_config = ExecutionConfig::<TestTypes>::for_config::<OverriddenAndOptIn>();
        for inspect in [false, true] {
            let interp = run_with(RunConfig::new(code), execution_config.clone(), inspect);
            assert_eq!(interp.stack(), [Word::from(2), Word::from(6)]);
            assert_eq!(interp.gas_remaining(), 10_000 - 4 * 3);
        }
    }

    #[test]
    fn repriced_instruction_is_fused_with_its_gas() {
        let (base, config) = (&osaka(), RepricedAdd::OPCODE_CONFIG);
        assert!(PATTERNS.iter().all(|pattern| fusable(config, base, pattern)));

        let code = [PUSH1, 2, PUSH1, 3, ADD, STOP];
        let execution_config = ExecutionConfig::<TestTypes>::for_config::<RepricedAdd>();
        for inspect in [false, true] {
            let config = RunConfig::new(code).gas_limit(17);
            let interp = run_with(config, execution_config.clone(), inspect);
            assert_eq!(interp.err, InstrStop::Stop);
            assert_eq!(interp.stack(), [Word::from(5)]);
            assert_eq!(interp.gas_remaining(), 0);

            let config = RunConfig::new(code).gas_limit(16);
            let interp = run_with(config, execution_config.clone(), inspect);
            assert_eq!(interp.err, InstrStop::OutOfGas);
        }
    }

    #[test]
    fn superinstructions_match_plain_handlers_of_custom_configs() {
        assert_matches_plain_handlers(0xc0f1, 1_500, |round| {
            let config = match round % 3 {
                0 => ExecutionConfig::for_config::<OverriddenAndOptIn>(),
                1 => ExecutionConfig::for_config::<RepricedAdd>(),
                _ => ExecutionConfig::for_config::<AddedOpcodeOptIn>(),
            };
            (SpecId::OSAKA, config)
        });
    }
}
