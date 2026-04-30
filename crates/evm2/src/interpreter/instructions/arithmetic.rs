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
    use crate::interpreter::{Word, instructions::tests::assert_stack};

    fn neg(value: u64) -> Word {
        Word::ZERO.wrapping_sub(Word::from(value))
    }

    #[test]
    fn add_opcode() {
        assert_stack!(ADD(1, 2), 3);
        assert_stack!(ADD(Word::MAX, 1), Word::ZERO);
        assert_stack!(ADD(Word::ZERO, Word::ZERO), Word::ZERO);
    }

    #[test]
    fn mul_opcode() {
        assert_stack!(MUL(3, 7), 21);
        assert_stack!(MUL(Word::MAX, 2), Word::MAX - Word::from(1));
        assert_stack!(MUL(123, Word::ZERO), Word::ZERO);
    }

    #[test]
    fn sub_opcode() {
        assert_stack!(SUB(7, 3), 4);
        assert_stack!(SUB(Word::ZERO, 1), Word::MAX);
        assert_stack!(SUB(9, 9), Word::ZERO);
    }

    #[test]
    fn div_opcode() {
        assert_stack!(DIV(7, 2), 3);
        assert_stack!(DIV(7, Word::ZERO), Word::ZERO);
        assert_stack!(DIV(Word::ZERO, 3), Word::ZERO);
    }

    #[test]
    fn sdiv_opcode() {
        assert_stack!(SDIV(neg(4), 2), neg(2));
        assert_stack!(SDIV(7, neg(2)), neg(3));
        assert_stack!(SDIV(7, Word::ZERO), Word::ZERO);
    }

    #[test]
    fn mod_opcode() {
        assert_stack!(MOD(7, 3), 1);
        assert_stack!(MOD(7, Word::ZERO), Word::ZERO);
        assert_stack!(MOD(9, 3), Word::ZERO);
    }

    #[test]
    fn smod_opcode() {
        assert_stack!(SMOD(neg(4), 3), neg(1));
        assert_stack!(SMOD(4, neg(3)), 1);
        assert_stack!(SMOD(4, Word::ZERO), Word::ZERO);
    }

    #[test]
    fn addmod_opcode() {
        assert_stack!(ADDMOD(5, 6, 7), 4);
        assert_stack!(ADDMOD(Word::MAX, 1, 9), 7);
        assert_stack!(ADDMOD(1, 2, Word::ZERO), Word::ZERO);
    }

    #[test]
    fn mulmod_opcode() {
        assert_stack!(MULMOD(5, 6, 7), 2);
        assert_stack!(MULMOD(Word::MAX, 2, 9), 3);
        assert_stack!(MULMOD(2, 3, Word::ZERO), Word::ZERO);
    }

    #[test]
    fn exp_opcode() {
        assert_stack!(EXP(2, 10), 1024);
        assert_stack!(EXP(5, Word::ZERO), 1);
        assert_stack!(EXP(Word::ZERO, 3), Word::ZERO);
    }

    #[test]
    fn signextend_opcode() {
        assert_stack!(SIGNEXTEND(Word::ZERO, 0x80), Word::MAX - Word::from(0x7f));
        assert_stack!(SIGNEXTEND(Word::ZERO, 0x7f), 0x7f);
        assert_stack!(SIGNEXTEND(31, 0x80), 0x80);
    }
}
