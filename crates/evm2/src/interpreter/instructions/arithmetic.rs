use super::{
    super::{CtrlRef, Gas, InstrErr, Result, Stack, State, Word},
    utils::{i256_div, i256_mod},
};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn add(a: &Word, b: &Word) -> Result<out> {
    *out = a.wrapping_add(*b);
}

#[instruction]
pub(in crate::interpreter) fn mul(a: &Word, b: &Word) -> Result<out> {
    *out = a.wrapping_mul(*b);
}

#[instruction]
pub(in crate::interpreter) fn sub(a: &Word, b: &Word) -> Result<out> {
    *out = a.wrapping_sub(*b);
}

#[instruction]
pub(in crate::interpreter) fn div(a: &Word, b: &Word) -> Result<out> {
    *out = if b.is_zero() { Word::ZERO } else { a.wrapping_div(*b) };
}

#[instruction]
pub(in crate::interpreter) fn sdiv(a: &Word, b: &Word) -> Result<out> {
    *out = i256_div(*a, *b);
}

#[instruction]
pub(in crate::interpreter) fn rem(a: &Word, b: &Word) -> Result<out> {
    *out = if b.is_zero() { Word::ZERO } else { a.wrapping_rem(*b) };
}

#[instruction]
pub(in crate::interpreter) fn smod(a: &Word, b: &Word) -> Result<out> {
    *out = i256_mod(*a, *b);
}

#[instruction]
pub(in crate::interpreter) fn addmod(a: &Word, b: &Word, n: &Word) -> Result<out> {
    *out = a.add_mod(*b, *n);
}

#[instruction]
pub(in crate::interpreter) fn mulmod(a: &Word, b: &Word, n: &Word) -> Result<out> {
    *out = a.mul_mod(*b, *n);
}

#[instruction]
pub(in crate::interpreter) fn exp(a: &Word, b: &Word) -> Result<out> {
    *out = a.pow(*b);
}

#[instruction]
pub(in crate::interpreter) fn signextend(ext: &Word, value: &Word) -> Result<out> {
    *out = *value;
    if *ext < Word::from(31) {
        let bit_index = (8 * ext.as_limbs()[0] + 7) as usize;
        let mask = (Word::from(1) << bit_index) - Word::from(1);
        *out = if value.bit(bit_index) { *value | !mask } else { *value & mask };
    }
}
