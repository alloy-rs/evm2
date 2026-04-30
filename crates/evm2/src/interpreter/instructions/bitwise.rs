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
    use crate::interpreter::{Word, instructions::tests::assert_stack};

    fn neg(value: u64) -> Word {
        Word::ZERO.wrapping_sub(Word::from(value))
    }

    #[test]
    fn lt_opcode() {
        assert_stack!(LT(1, 2), 1);
        assert_stack!(LT(2, 1), Word::ZERO);
        assert_stack!(LT(2, 2), Word::ZERO);
    }

    #[test]
    fn gt_opcode() {
        assert_stack!(GT(2, 1), 1);
        assert_stack!(GT(1, 2), Word::ZERO);
        assert_stack!(GT(2, 2), Word::ZERO);
    }

    #[test]
    fn slt_opcode() {
        assert_stack!(SLT(Word::MAX, Word::ZERO), 1);
        assert_stack!(SLT(Word::ZERO, Word::MAX), Word::ZERO);
        assert_stack!(SLT(neg(2), neg(1)), 1);
    }

    #[test]
    fn sgt_opcode() {
        assert_stack!(SGT(Word::ZERO, Word::MAX), 1);
        assert_stack!(SGT(Word::MAX, Word::ZERO), Word::ZERO);
        assert_stack!(SGT(neg(1), neg(2)), 1);
    }

    #[test]
    fn eq_opcode() {
        assert_stack!(EQ(3, 3), 1);
        assert_stack!(EQ(3, 4), Word::ZERO);
        assert_stack!(EQ(Word::MAX, Word::MAX), 1);
    }

    #[test]
    fn iszero_opcode() {
        assert_stack!(ISZERO(Word::ZERO), 1);
        assert_stack!(ISZERO(1), Word::ZERO);
        assert_stack!(ISZERO(Word::MAX), Word::ZERO);
    }

    #[test]
    fn and_opcode() {
        assert_stack!(AND(0b1100, 0b1010), 0b1000);
        assert_stack!(AND(Word::MAX, 0x55), 0x55);
        assert_stack!(AND(Word::ZERO, Word::MAX), Word::ZERO);
    }

    #[test]
    fn or_opcode() {
        assert_stack!(OR(0b1100, 0b1010), 0b1110);
        assert_stack!(OR(Word::ZERO, 0x55), 0x55);
        assert_stack!(OR(Word::MAX, Word::ZERO), Word::MAX);
    }

    #[test]
    fn xor_opcode() {
        assert_stack!(XOR(0b1100, 0b1010), 0b0110);
        assert_stack!(XOR(Word::MAX, Word::MAX), Word::ZERO);
        assert_stack!(XOR(Word::ZERO, 0x55), 0x55);
    }

    #[test]
    fn not_opcode() {
        assert_stack!(NOT(Word::ZERO), Word::MAX);
        assert_stack!(NOT(Word::MAX), Word::ZERO);
        assert_stack!(NOT(0xff), Word::MAX - Word::from(0xff));
    }

    #[test]
    fn byte_opcode() {
        assert_stack!(BYTE(31, 0x1234), 0x34);
        assert_stack!(BYTE(30, 0x1234), 0x12);
        assert_stack!(BYTE(32, 0x1234), Word::ZERO);
    }

    #[test]
    fn shl_opcode() {
        assert_stack!(SHL(8, 1), 256);
        assert_stack!(SHL(Word::ZERO, 7), 7);
        assert_stack!(SHL(256, 1), Word::ZERO);
    }

    #[test]
    fn shr_opcode() {
        assert_stack!(SHR(8, 256), 1);
        assert_stack!(SHR(Word::ZERO, 7), 7);
        assert_stack!(SHR(256, Word::MAX), Word::ZERO);
    }

    #[test]
    fn sar_opcode() {
        assert_stack!(SAR(1, Word::MAX - Word::from(1)), Word::MAX);
        assert_stack!(SAR(1, 4), 2);
        assert_stack!(SAR(256, Word::MAX), Word::MAX);
    }

    #[test]
    fn clz_opcode() {
        assert_stack!(CLZ(1), 255);
        assert_stack!(CLZ(Word::ZERO), 256);
        assert_stack!(CLZ(Word::MAX), Word::ZERO);
    }
}
