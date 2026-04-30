use super::i256::{i256_div, i256_mod};
use crate::interpreter::{CtrlRef, Gas, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn add(a: &Word, b: &Word) -> out {
    *out = a.wrapping_add(*b);
}

#[instruction]
pub(in crate::interpreter) fn mul(a: &Word, b: &Word) -> out {
    *out = a.wrapping_mul(*b);
}

#[instruction]
pub(in crate::interpreter) fn sub(a: &Word, b: &Word) -> out {
    *out = a.wrapping_sub(*b);
}

#[instruction]
pub(in crate::interpreter) fn div(a: &Word, b: &Word) -> out {
    *out = if b.is_zero() { Word::ZERO } else { a.wrapping_div(*b) };
}

#[instruction]
pub(in crate::interpreter) fn sdiv(a: &Word, b: &Word) -> out {
    *out = i256_div(*a, *b);
}

#[instruction]
pub(in crate::interpreter) fn rem(a: &Word, b: &Word) -> out {
    *out = if b.is_zero() { Word::ZERO } else { a.wrapping_rem(*b) };
}

#[instruction]
pub(in crate::interpreter) fn smod(a: &Word, b: &Word) -> out {
    *out = i256_mod(*a, *b);
}

#[instruction]
pub(in crate::interpreter) fn addmod(a: &Word, b: &Word, n: &Word) -> out {
    *out = a.add_mod(*b, *n);
}

#[instruction]
pub(in crate::interpreter) fn mulmod(a: &Word, b: &Word, n: &Word) -> out {
    *out = a.mul_mod(*b, *n);
}

#[instruction]
pub(in crate::interpreter) fn exp(a: &Word, b: &Word) -> out {
    *out = a.wrapping_pow(*b);
}

#[instruction]
pub(in crate::interpreter) fn signextend(ext: &Word, value: &Word) -> out {
    *out = *value;
    if *ext < Word::from(31) {
        let bit_index = (8 * ext.as_limbs()[0] + 7) as usize;
        let mask = (Word::from(1) << bit_index) - Word::from(1);
        *out = if value.bit(bit_index) { *value | !mask } else { *value & mask };
    }
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
    fn arithmetic_opcodes() {
        assert_op(&[Word::from(1), Word::from(2)], op::ADD, Word::from(3));
        assert_op(&[Word::from(3), Word::from(7)], op::MUL, Word::from(21));
        assert_op(&[Word::from(7), Word::from(3)], op::SUB, Word::from(4));
        assert_op(&[Word::from(7), Word::from(2)], op::DIV, Word::from(3));
        assert_op(&[Word::from(7), Word::ZERO], op::DIV, Word::ZERO);

        let neg_four = Word::ZERO.wrapping_sub(Word::from(4));
        let neg_two = Word::ZERO.wrapping_sub(Word::from(2));
        assert_op(&[neg_four, Word::from(2)], op::SDIV, neg_two);
        assert_op(&[Word::from(7), Word::from(3)], op::MOD, Word::from(1));
        assert_op(&[neg_four, Word::from(3)], op::SMOD, Word::ZERO.wrapping_sub(Word::from(1)));
        assert_op(&[Word::from(5), Word::from(6), Word::from(7)], op::ADDMOD, Word::from(4));
        assert_op(&[Word::from(5), Word::from(6), Word::from(7)], op::MULMOD, Word::from(2));
        assert_op(&[Word::from(2), Word::from(10)], op::EXP, Word::from(1024));
        assert_op(&[Word::ZERO, Word::from(0x80)], op::SIGNEXTEND, Word::MAX - Word::from(0x7f));
    }
}
