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
    use crate::interpreter::{Word, instructions::tests::assert_stack, op};

    fn neg(value: u64) -> Word {
        Word::ZERO.wrapping_sub(Word::from(value))
    }

    #[test]
    fn lt_opcode() {
        assert_stack(&[Word::from(1), Word::from(2)], op::LT, &[Word::from(1)]);
        assert_stack(&[Word::from(2), Word::from(1)], op::LT, &[Word::ZERO]);
        assert_stack(&[Word::from(2), Word::from(2)], op::LT, &[Word::ZERO]);
    }

    #[test]
    fn gt_opcode() {
        assert_stack(&[Word::from(2), Word::from(1)], op::GT, &[Word::from(1)]);
        assert_stack(&[Word::from(1), Word::from(2)], op::GT, &[Word::ZERO]);
        assert_stack(&[Word::from(2), Word::from(2)], op::GT, &[Word::ZERO]);
    }

    #[test]
    fn slt_opcode() {
        assert_stack(&[Word::MAX, Word::ZERO], op::SLT, &[Word::from(1)]);
        assert_stack(&[Word::ZERO, Word::MAX], op::SLT, &[Word::ZERO]);
        assert_stack(&[neg(2), neg(1)], op::SLT, &[Word::from(1)]);
    }

    #[test]
    fn sgt_opcode() {
        assert_stack(&[Word::ZERO, Word::MAX], op::SGT, &[Word::from(1)]);
        assert_stack(&[Word::MAX, Word::ZERO], op::SGT, &[Word::ZERO]);
        assert_stack(&[neg(1), neg(2)], op::SGT, &[Word::from(1)]);
    }

    #[test]
    fn eq_opcode() {
        assert_stack(&[Word::from(3), Word::from(3)], op::EQ, &[Word::from(1)]);
        assert_stack(&[Word::from(3), Word::from(4)], op::EQ, &[Word::ZERO]);
        assert_stack(&[Word::MAX, Word::MAX], op::EQ, &[Word::from(1)]);
    }

    #[test]
    fn iszero_opcode() {
        assert_stack(&[Word::ZERO], op::ISZERO, &[Word::from(1)]);
        assert_stack(&[Word::from(1)], op::ISZERO, &[Word::ZERO]);
        assert_stack(&[Word::MAX], op::ISZERO, &[Word::ZERO]);
    }

    #[test]
    fn and_opcode() {
        assert_stack(&[Word::from(0b1100), Word::from(0b1010)], op::AND, &[Word::from(0b1000)]);
        assert_stack(&[Word::MAX, Word::from(0x55)], op::AND, &[Word::from(0x55)]);
        assert_stack(&[Word::ZERO, Word::MAX], op::AND, &[Word::ZERO]);
    }

    #[test]
    fn or_opcode() {
        assert_stack(&[Word::from(0b1100), Word::from(0b1010)], op::OR, &[Word::from(0b1110)]);
        assert_stack(&[Word::ZERO, Word::from(0x55)], op::OR, &[Word::from(0x55)]);
        assert_stack(&[Word::MAX, Word::ZERO], op::OR, &[Word::MAX]);
    }

    #[test]
    fn xor_opcode() {
        assert_stack(&[Word::from(0b1100), Word::from(0b1010)], op::XOR, &[Word::from(0b0110)]);
        assert_stack(&[Word::MAX, Word::MAX], op::XOR, &[Word::ZERO]);
        assert_stack(&[Word::ZERO, Word::from(0x55)], op::XOR, &[Word::from(0x55)]);
    }

    #[test]
    fn not_opcode() {
        assert_stack(&[Word::ZERO], op::NOT, &[Word::MAX]);
        assert_stack(&[Word::MAX], op::NOT, &[Word::ZERO]);
        assert_stack(&[Word::from(0xff)], op::NOT, &[Word::MAX - Word::from(0xff)]);
    }

    #[test]
    fn byte_opcode() {
        assert_stack(&[Word::from(31), Word::from(0x1234)], op::BYTE, &[Word::from(0x34)]);
        assert_stack(&[Word::from(30), Word::from(0x1234)], op::BYTE, &[Word::from(0x12)]);
        assert_stack(&[Word::from(32), Word::from(0x1234)], op::BYTE, &[Word::ZERO]);
    }

    #[test]
    fn shl_opcode() {
        assert_stack(&[Word::from(8), Word::from(1)], op::SHL, &[Word::from(256)]);
        assert_stack(&[Word::ZERO, Word::from(7)], op::SHL, &[Word::from(7)]);
        assert_stack(&[Word::from(256), Word::from(1)], op::SHL, &[Word::ZERO]);
    }

    #[test]
    fn shr_opcode() {
        assert_stack(&[Word::from(8), Word::from(256)], op::SHR, &[Word::from(1)]);
        assert_stack(&[Word::ZERO, Word::from(7)], op::SHR, &[Word::from(7)]);
        assert_stack(&[Word::from(256), Word::MAX], op::SHR, &[Word::ZERO]);
    }

    #[test]
    fn sar_opcode() {
        assert_stack(&[Word::from(1), Word::MAX - Word::from(1)], op::SAR, &[Word::MAX]);
        assert_stack(&[Word::from(1), Word::from(4)], op::SAR, &[Word::from(2)]);
        assert_stack(&[Word::from(256), Word::MAX], op::SAR, &[Word::MAX]);
    }

    #[test]
    fn clz_opcode() {
        assert_stack(&[Word::from(1)], op::CLZ, &[Word::from(255)]);
        assert_stack(&[Word::ZERO], op::CLZ, &[Word::from(256)]);
        assert_stack(&[Word::MAX], op::CLZ, &[Word::ZERO]);
    }
}
