//! Instruction decoding.

use crate::{
    SpecId,
    opcode::{OpCode, op},
};
use alloy_primitives::hex;
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
    /// [`OpCode::immediate_size`] for a `PUSH` that the end of the bytecode cuts short, and empty
    /// for a `DUPN`, `SWAPN` or `EXCHANGE` that has no valid immediate.
    pub immediate: &'a [u8],
}

impl Instruction<'_> {
    /// Returns the offset of the byte after the instruction.
    #[inline]
    pub const fn end_pc(&self) -> usize {
        self.pc + 1 + self.immediate.len()
    }
}

impl<'a> Instruction<'a> {
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
            match (opcode.get(), immediate) {
                (op::DUPN | op::SWAPN, &[imm]) if let Some(n) = decode_single(imm) => {
                    write!(f, " {n}")
                }
                (op::EXCHANGE, &[imm]) if let Some((n, m)) = decode_pair(imm) => {
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
///   byte is valid. Otherwise the instruction has no immediate, and the byte after it is the next
///   instruction. Execution halts at such an instruction.
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
        let imm_len = match opcode {
            op::PUSH1..=op::PUSH32 => (opcode - op::PUSH0) as usize,
            op::DUPN | op::SWAPN | op::EXCHANGE if self.eip8024 => {
                let valid = self.code.get(pc + 1).is_some_and(|&imm| match opcode {
                    op::EXCHANGE => decode_pair(imm).is_some(),
                    _ => decode_single(imm).is_some(),
                });
                valid as usize
            }
            _ => 0,
        };
        let end = self.code.len().min(pc + 1 + imm_len);
        self.pc = end;
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

/// Decodes a `DUPN`/`SWAPN` immediate byte into a stack index.
///
/// Returns `None` if the immediate is in the invalid range `[91, 127]`.
pub const fn decode_single(x: u8) -> Option<u8> {
    if x <= 90 || x >= 128 { Some(x.wrapping_add(145)) } else { None }
}

/// Decodes an `EXCHANGE` immediate byte into a pair of stack indices `(n, m)`.
///
/// Returns `None` if the immediate is in the invalid range `[82, 127]`.
pub const fn decode_pair(x: u8) -> Option<(u8, u8)> {
    if x > 81 && x < 128 {
        return None;
    }
    let k = (x ^ 143) as u16;
    let q = (k / 16) as u8;
    let r = (k % 16) as u8;
    if q < r { Some((q + 1, r + 1)) } else { Some((r + 1, 29 - q)) }
}

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
    fn invalid_eip8024_immediate_is_not_consumed() {
        // 0x5b is a JUMPDEST and 0x60 a PUSH1: neither is an immediate of an invalid encoding.
        let code = [op::DUPN, 0x5b, op::EXCHANGE, 0x52, op::SWAPN, 0x60, 0xff, op::SWAPN];
        let insts = decode(&code, SpecId::AMSTERDAM);
        assert_eq!(
            insts,
            [
                (0, op::DUPN, vec![]),
                (1, op::JUMPDEST, vec![]),
                (2, op::EXCHANGE, vec![]),
                (3, 0x52, vec![]),
                (4, op::SWAPN, vec![]),
                (5, op::PUSH1, vec![0xff]),
                (7, op::SWAPN, vec![]),
            ]
        );
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
}
