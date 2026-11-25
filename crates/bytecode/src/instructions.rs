//! Instruction decoding.

use crate::{
    SpecId,
    eip8024::{decode_pair, decode_single},
    opcode::{OpCode, op},
};
use alloy_primitives::{U256, hex};
use core::{fmt, iter::FusedIterator};

/// A decoded instruction: an opcode byte and its immediate data.
///
/// Created by [`Instructions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction<'a> {
    /// The offset of the opcode byte in the bytecode.
    pub pc: usize,
    /// The opcode. It may be a byte that is not an opcode, see [`OpCode::is_valid`].
    pub opcode: OpCode,
    /// The immediate data that follows the opcode byte.
    ///
    /// This is empty for opcodes without immediate data. It is shorter than
    /// [`OpCode::immediate_size`] for a `PUSH`, `DUPN`, `SWAPN` or `EXCHANGE` that the end of the
    /// bytecode cuts short.
    ///
    /// For an enabled `DUPN`, `SWAPN` or `EXCHANGE` whose byte is not a valid immediate, this is
    /// that byte, but the instruction does not consume it: the byte is also the next instruction,
    /// and `stack_io` is `None`. The interpreter halts at such an instruction.
    pub immediate: &'a [u8],
}

impl<'a> Instruction<'a> {
    /// Returns the value the instruction pushes if it is a `PUSH`, or `None` for any other
    /// opcode.
    ///
    /// The immediate data is read as the EVM reads it: bytes that the end of the bytecode cuts
    /// off are zeros on the right, so `PUSH2 0x12` at the end is `0x1200`. `PUSH0` is `Some(0)`
    /// whether or not the spec enables it; use [`OpCode::is_enabled`] to tell.
    pub fn push_value(&self) -> Option<U256> {
        let size = match self.opcode.get() {
            op::PUSH0 => return Some(U256::ZERO),
            opcode @ op::PUSH1..=op::PUSH32 => (opcode - op::PUSH0) as usize,
            _ => return None,
        };
        let mut word = [0; 32];
        let len = self.immediate.len().min(size);
        word[..len].copy_from_slice(&self.immediate[..len]);
        Some(U256::from_be_slice(&word[..size]))
    }

    /// Returns the number of stack items the instruction pops and pushes, or `None` if it is a
    /// `DUPN`, `SWAPN` or `EXCHANGE` with an invalid immediate.
    ///
    /// Those three read their depths from the immediate. A missing immediate reads as zero, as
    /// the zero padding after the bytecode does. The result does not depend on the spec, so check
    /// [`OpCode::is_enabled`] first. A byte that is not an opcode pops and pushes nothing.
    pub fn stack_io(&self) -> Option<(u8, u8)> {
        let immediate = self.immediate.first().copied().unwrap_or(0);
        match self.opcode.get() {
            op::DUPN => decode_single(immediate).map(|n| (n, n + 1)),
            op::SWAPN => decode_single(immediate).map(|n| (n + 1, n + 1)),
            op::EXCHANGE => decode_pair(immediate).map(|(_, m)| (m + 1, m + 1)),
            _ => Some(self.opcode.input_output()),
        }
    }

    /// Returns a [`Display`](fmt::Display) of the instruction as `spec_id` names it.
    ///
    /// This differs from the instruction's own `Display` in two ways: the opcode `0x44` is
    /// `PREVRANDAO` from Paris on, and `DUPN`, `SWAPN` and `EXCHANGE` print the stack indices
    /// they decode to, such as `DUPN 17` and `EXCHANGE 1, 2`, instead of the immediate byte.
    pub fn display(&self, spec_id: SpecId) -> impl fmt::Display + use<'a> {
        let Self { opcode, immediate, .. } = *self;
        fmt::from_fn(move |f| {
            if !opcode.is_valid() {
                return write!(f, "UNKNOWN(0x{:02x})", opcode.get());
            }
            f.write_str(opcode.name_in(spec_id))?;
            // An immediate that the end of the code cuts off reads as zero.
            let imm = immediate.first().copied().unwrap_or(0);
            let enabled = opcode.is_enabled(spec_id);
            match (opcode.get(), immediate) {
                (op::DUPN | op::SWAPN, _) if enabled && let Some(n) = decode_single(imm) => {
                    write!(f, " {n}")
                }
                (op::EXCHANGE, _) if enabled && let Some((n, m)) = decode_pair(imm) => {
                    write!(f, " {n}, {m}")
                }
                (_, []) => Ok(()),
                (_, immediate) => write!(f, " {}", hex::encode_prefixed(immediate)),
            }
        })
    }
}

impl fmt::Display for Instruction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.opcode.is_valid() {
            true => f.write_str(self.opcode.as_str())?,
            false => write!(f, "UNKNOWN(0x{:02x})", self.opcode.get())?,
        }
        if !self.immediate.is_empty() {
            write!(f, " {}", hex::encode_prefixed(self.immediate))?;
        }
        Ok(())
    }
}

/// An iterator over the instructions of a bytecode.
///
/// The decoding agrees with [`analyze_legacy`](crate::analyze_legacy): every valid jump
/// destination is the start of an instruction.
///
/// - `PUSH1..=PUSH32` take their immediate data. The last instruction can have a shorter one.
/// - `DUPN`, `SWAPN` and `EXCHANGE` take a one byte immediate when `spec_id` enables them and the
///   byte is valid. With an invalid byte the instruction does not take it: the byte is the next
///   instruction, and [`Instruction::immediate`] only shows it. Before the spec enables them, the
///   three have no immediate.
/// - Every other byte is an instruction without immediate data.
#[derive(Clone, Debug)]
pub struct Instructions<'a> {
    code: &'a [u8],
    pc: usize,
    eip8024: bool,
}

impl<'a> Instructions<'a> {
    /// Creates an iterator over the instructions of `code`.
    #[inline]
    pub const fn new(code: &'a [u8], spec_id: SpecId) -> Self {
        let eip8024 = OpCode::new_or_unknown(op::DUPN).is_enabled(spec_id);
        Self { code, pc: 0, eip8024 }
    }
}

impl<'a> Iterator for Instructions<'a> {
    type Item = Instruction<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let pc = self.pc;
        let &opcode = self.code.get(pc)?;
        // The length of the immediate data, and whether the instruction takes it.
        let (imm_len, takes) = match opcode {
            op::PUSH1..=op::PUSH32 => ((opcode - op::PUSH0) as usize, true),
            op::DUPN | op::SWAPN | op::EXCHANGE if self.eip8024 => {
                let valid = self.code.get(pc + 1).is_none_or(|&imm| match opcode {
                    op::EXCHANGE => decode_pair(imm).is_some(),
                    _ => decode_single(imm).is_some(),
                });
                (1, valid)
            }
            _ => (0, true),
        };
        let end = self.code.len().min(pc + 1 + imm_len);
        self.pc = if takes { end } else { pc + 1 };
        Some(Instruction {
            pc,
            opcode: OpCode::new_or_unknown(opcode),
            immediate: &self.code[pc + 1..end],
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.code.len() - self.pc;
        (len.min(1), Some(len))
    }
}

impl FusedIterator for Instructions<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze_legacy;
    use alloc::{string::ToString, vec, vec::Vec};

    fn decode(code: &[u8], spec_id: SpecId) -> Vec<(usize, u8, Vec<u8>)> {
        Instructions::new(code, spec_id)
            .map(|inst| (inst.pc, inst.opcode.get(), inst.immediate.to_vec()))
            .collect()
    }

    #[test]
    fn push_data() {
        let code = [op::PUSH0, op::PUSH1, 0x69, op::PUSH2, 0x01, 0x02, op::ADD];
        assert_eq!(
            decode(&code, SpecId::OSAKA),
            [
                (0, op::PUSH0, vec![]),
                (1, op::PUSH1, vec![0x69]),
                (3, op::PUSH2, vec![0x01, 0x02]),
                (6, op::ADD, vec![]),
            ]
        );
    }

    #[test]
    fn truncated_push_is_short() {
        let code = [op::PUSH1, 0x01, op::PUSH3, 0xaa, 0xbb];
        assert_eq!(
            decode(&code, SpecId::OSAKA),
            [(0, op::PUSH1, vec![0x01]), (2, op::PUSH3, vec![0xaa, 0xbb])]
        );
        assert_eq!(decode(&[op::PUSH32], SpecId::OSAKA), [(0, op::PUSH32, vec![])]);
    }

    #[test]
    fn eip8024_immediates_follow_the_spec() {
        // DUPN 0x00, SWAPN 0x80, EXCHANGE 0x01, STOP.
        let code = [op::DUPN, 0x00, op::SWAPN, 0x80, op::EXCHANGE, 0x01, op::STOP];
        assert_eq!(
            decode(&code, SpecId::AMSTERDAM),
            [
                (0, op::DUPN, vec![0x00]),
                (2, op::SWAPN, vec![0x80]),
                (4, op::EXCHANGE, vec![0x01]),
                (6, op::STOP, vec![]),
            ]
        );
        // Before the fork the opcodes have no immediate, so the immediates are instructions.
        assert_eq!(
            decode(&code, SpecId::OSAKA),
            [
                (0, op::DUPN, vec![]),
                (1, op::STOP, vec![]),
                (2, op::SWAPN, vec![]),
                (3, 0x80, vec![]),
                (4, op::EXCHANGE, vec![]),
                (5, op::ADD, vec![]),
                (6, op::STOP, vec![]),
            ]
        );
    }

    #[test]
    fn invalid_eip8024_immediate_is_shown_but_not_consumed() {
        // 0x5b is a JUMPDEST and 0x60 a PUSH1: neither is an immediate of an invalid encoding.
        let code = [op::DUPN, 0x5b, op::EXCHANGE, 0x52, op::SWAPN, 0x60, 0xff, op::SWAPN];
        let insts = Instructions::new(&code, SpecId::AMSTERDAM).collect::<Vec<_>>();
        assert_eq!(
            insts.iter().map(|i| (i.pc, i.opcode.get(), i.immediate.to_vec())).collect::<Vec<_>>(),
            [
                (0, op::DUPN, vec![0x5b]),
                (1, op::JUMPDEST, vec![]),
                (2, op::EXCHANGE, vec![0x52]),
                (3, 0x52, vec![]),
                (4, op::SWAPN, vec![0x60]),
                (5, op::PUSH1, vec![0xff]),
                (7, op::SWAPN, vec![]),
            ]
        );
        // The invalid ones have no stack effect, and the one at the end reads a zero.
        let io = insts.iter().map(Instruction::stack_io).collect::<Vec<_>>();
        assert_eq!(io[0], None);
        assert_eq!(io[2], None);
        assert_eq!(io[4], None);
        assert_eq!(io[6], Some((146, 146)));
        assert_eq!(insts[0].to_string(), "DUPN 0x5b");
    }

    #[test]
    fn jumpdests_agree_with_analysis() {
        // Every byte value after every opcode that can take an immediate, under both specs.
        for spec_id in [SpecId::OSAKA, SpecId::AMSTERDAM] {
            for first in [op::DUPN, op::SWAPN, op::EXCHANGE, op::PUSH1, op::PUSH2] {
                for second in 0..=u8::MAX {
                    let code = [first, second, op::JUMPDEST, op::PUSH1, op::JUMPDEST, op::JUMPDEST];
                    let table = analyze_legacy(&code);
                    let jumpdests = Instructions::new(&code, spec_id)
                        .filter(|inst| inst.opcode.is_jumpdest())
                        .map(|inst| inst.pc)
                        .collect::<Vec<_>>();
                    let expected =
                        (0..code.len()).filter(|&pc| table.is_valid(pc)).collect::<Vec<_>>();
                    assert_eq!(jumpdests, expected, "{spec_id:?} {first:#x} {second:#x}");
                }
            }
        }
    }

    #[test]
    fn display() {
        let code = [op::PUSH2, 0x00, 0x3e, op::JUMPDEST, 0x0c, op::PUSH2, 0x01];
        let insts =
            Instructions::new(&code, SpecId::OSAKA).map(|i| i.to_string()).collect::<Vec<_>>();
        assert_eq!(insts, ["PUSH2 0x003e", "JUMPDEST", "UNKNOWN(0x0c)", "PUSH2 0x01"]);
    }

    #[test]
    fn display_reads_a_cut_off_immediate_as_zero() {
        let show = |code: &[u8], spec_id| {
            Instructions::new(code, spec_id).last().unwrap().display(spec_id).to_string()
        };
        assert_eq!(show(&[op::DUPN], SpecId::AMSTERDAM), "DUPN 145");
        assert_eq!(show(&[op::SWAPN], SpecId::AMSTERDAM), "SWAPN 145");
        assert_eq!(show(&[op::EXCHANGE], SpecId::AMSTERDAM), "EXCHANGE 9, 16");
        // Not enabled before Amsterdam, and an invalid byte is shown as it is.
        assert_eq!(show(&[op::DUPN], SpecId::OSAKA), "DUPN");
        assert_eq!(show(&[op::DUPN, 0x5b], SpecId::AMSTERDAM), "JUMPDEST");
        let invalid = Instructions::new(&[op::DUPN, 0x5b], SpecId::AMSTERDAM).next().unwrap();
        assert_eq!(invalid.display(SpecId::AMSTERDAM).to_string(), "DUPN 0x5b");
    }

    #[test]
    fn display_names_opcodes_and_operands_by_spec() {
        let code = [
            op::DIFFICULTY,
            op::DUPN,
            0x80,
            op::SWAPN,
            0x80,
            op::EXCHANGE,
            0x8e,
            op::PUSH2,
            0x00,
            0x3e,
            0x0c,
        ];
        let show = |spec_id| {
            Instructions::new(&code, spec_id)
                .map(|inst| inst.display(spec_id).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            show(SpecId::AMSTERDAM),
            ["PREVRANDAO", "DUPN 17", "SWAPN 17", "EXCHANGE 1, 2", "PUSH2 0x003e", "UNKNOWN(0x0c)"]
        );
        assert_eq!(show(SpecId::LONDON)[0], "DIFFICULTY");
        assert_eq!(show(SpecId::MERGE)[0], "PREVRANDAO");
        // The instruction's own `Display` names the table entry and prints the raw immediate.
        let raw =
            Instructions::new(&code, SpecId::AMSTERDAM).map(|i| i.to_string()).collect::<Vec<_>>();
        assert_eq!(raw[..4], ["DIFFICULTY", "DUPN 0x80", "SWAPN 0x80", "EXCHANGE 0x8e"]);
    }

    #[test]
    fn push_value_pads_truncated_data_on_the_right() {
        let value =
            |code: &[u8]| Instructions::new(code, SpecId::OSAKA).next().unwrap().push_value();
        assert_eq!(value(&[op::PUSH0]), Some(U256::ZERO));
        assert_eq!(value(&[op::PUSH1, 0x7f]), Some(U256::from(0x7f)));
        assert_eq!(value(&[op::PUSH2, 0x01, 0x02]), Some(U256::from(0x0102)));
        // The code ends inside the immediate: the missing bytes are zeros after the present ones.
        assert_eq!(value(&[op::PUSH2, 0x12]), Some(U256::from(0x1200)));
        assert_eq!(value(&[op::PUSH2]), Some(U256::ZERO));
        assert_eq!(value(&[op::PUSH3, 0xaa, 0xbb]), Some(U256::from(0xaabb00)));
        assert_eq!(value(&[op::PUSH32, 0xff]), Some(U256::from(0xff) << 248));
        let mut full = [0xab; 33];
        full[0] = op::PUSH32;
        assert_eq!(value(&full), Some(U256::from_be_bytes([0xab; 32])));
        // Not pushes.
        assert_eq!(value(&[op::DUP1]), None);
        assert_eq!(value(&[op::DUPN, 0x80]), None);
        assert_eq!(value(&[0x0c]), None);
    }

    #[test]
    fn stack_io_decodes_relative_stack_opcodes() {
        let io =
            |code: &[u8]| Instructions::new(code, SpecId::AMSTERDAM).next().unwrap().stack_io();
        assert_eq!(io(&[op::ADD]), Some((2, 1)));
        assert_eq!(io(&[op::PUSH1, 0]), Some((0, 1)));
        assert_eq!(io(&[op::DUP16]), Some((16, 17)));
        assert_eq!(io(&[op::SWAP16]), Some((17, 17)));
        assert_eq!(io(&[op::CALL]), Some((7, 1)));
        // DUPN 17 reads 17 and writes 18; SWAPN 17 touches 18; EXCHANGE 1, 2 touches 3.
        assert_eq!(io(&[op::DUPN, 0x80]), Some((17, 18)));
        assert_eq!(io(&[op::SWAPN, 0x80]), Some((18, 18)));
        assert_eq!(io(&[op::EXCHANGE, 0x8e]), Some((3, 3)));
        assert_eq!(io(&[op::DUPN, 0xff]), Some((144, 145)));
        // A missing immediate reads as zero: DUPN 145.
        assert_eq!(io(&[op::DUPN]), Some((145, 146)));
        // Invalid immediates.
        assert_eq!(io(&[op::DUPN, 91]), None);
        assert_eq!(io(&[op::SWAPN, 127]), None);
        assert_eq!(io(&[op::EXCHANGE, 82]), None);
        // A byte that is not an opcode.
        assert_eq!(io(&[0x0c]), Some((0, 0)));
    }
}
