use super::{i256::i256_cmp, utils::as_usize};
use crate::interpreter::{CtrlRef, Gas, Result, Stack, State, Word};
use core::cmp::Ordering;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn lt(a: &Word, b: &Word) -> out {
    *out = Word::from(a < b);
}

#[instruction]
pub(in crate::interpreter) fn gt(a: &Word, b: &Word) -> out {
    *out = Word::from(a > b);
}

#[instruction]
pub(in crate::interpreter) fn slt(a: &Word, b: &Word) -> out {
    *out = Word::from(i256_cmp(a, b) == Ordering::Less);
}

#[instruction]
pub(in crate::interpreter) fn sgt(a: &Word, b: &Word) -> out {
    *out = Word::from(i256_cmp(a, b) == Ordering::Greater);
}

#[instruction]
pub(in crate::interpreter) fn eq(a: &Word, b: &Word) -> out {
    *out = Word::from(a == b);
}

#[instruction]
pub(in crate::interpreter) fn iszero(value: &Word) -> out {
    *out = Word::from(value.is_zero());
}

#[instruction]
pub(in crate::interpreter) fn bitand(a: &Word, b: &Word) -> out {
    *out = *a & *b;
}

#[instruction]
pub(in crate::interpreter) fn bitor(a: &Word, b: &Word) -> out {
    *out = *a | *b;
}

#[instruction]
pub(in crate::interpreter) fn bitxor(a: &Word, b: &Word) -> out {
    *out = *a ^ *b;
}

#[instruction]
pub(in crate::interpreter) fn not(value: &Word) -> out {
    *out = !*value;
}

#[instruction]
pub(in crate::interpreter) fn byte(index: &Word, value: &Word) -> out {
    let index = as_usize(*index).unwrap_or(usize::MAX);
    *out = if index < 32 { Word::from(value.byte(31 - index)) } else { Word::ZERO };
}

#[instruction]
pub(in crate::interpreter) fn shl(shift: &Word, value: &Word) -> out {
    let shift = as_usize(*shift).unwrap_or(usize::MAX);
    *out = if shift < 256 { *value << shift } else { Word::ZERO };
}

#[instruction]
pub(in crate::interpreter) fn shr(shift: &Word, value: &Word) -> out {
    let shift = as_usize(*shift).unwrap_or(usize::MAX);
    *out = if shift < 256 { *value >> shift } else { Word::ZERO };
}

#[instruction]
pub(in crate::interpreter) fn sar(shift: &Word, value: &Word) -> out {
    let shift = as_usize(*shift).unwrap_or(usize::MAX);
    *out = if shift < 256 {
        value.arithmetic_shr(shift)
    } else if value.bit(255) {
        Word::MAX
    } else {
        Word::ZERO
    };
}

#[instruction]
pub(in crate::interpreter) fn clz(value: &Word) -> out {
    *out = Word::from(value.leading_zeros());
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{InstrErr, Word, instructions::tests::run_stack, op};

    fn assert_op(inputs: &[Word], opcode: u8, expected: Word) {
        let interpreter = run_stack(inputs, opcode);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [expected]);
    }

    #[test]
    fn bitwise_opcodes() {
        assert_op(&[Word::from(1), Word::from(2)], op::LT, Word::from(1));
        assert_op(&[Word::from(2), Word::from(1)], op::GT, Word::from(1));
        assert_op(&[Word::MAX, Word::ZERO], op::SLT, Word::from(1));
        assert_op(&[Word::ZERO, Word::MAX], op::SGT, Word::from(1));
        assert_op(&[Word::from(3), Word::from(3)], op::EQ, Word::from(1));
        assert_op(&[Word::ZERO], op::ISZERO, Word::from(1));
        assert_op(&[Word::from(0b1100), Word::from(0b1010)], op::AND, Word::from(0b1000));
        assert_op(&[Word::from(0b1100), Word::from(0b1010)], op::OR, Word::from(0b1110));
        assert_op(&[Word::from(0b1100), Word::from(0b1010)], op::XOR, Word::from(0b0110));
        assert_op(&[Word::ZERO], op::NOT, Word::MAX);
        assert_op(&[Word::from(31), Word::from(0x1234)], op::BYTE, Word::from(0x34));
        assert_op(&[Word::from(8), Word::from(1)], op::SHL, Word::from(256));
        assert_op(&[Word::from(8), Word::from(256)], op::SHR, Word::from(1));
        assert_op(&[Word::from(1), Word::MAX - Word::from(1)], op::SAR, Word::MAX);
        assert_op(&[Word::from(1)], op::CLZ, Word::from(255));
    }
}
