//! Shared utility helpers.

use crate::interpreter::{InstrStop, Result, Word};
use alloy_primitives::{Address, B256};
use core::hint::cold_path;

/// Returns the number of EVM words needed for `len` bytes.
#[inline]
pub const fn num_words(len: usize) -> usize {
    len.div_ceil(32)
}

/// Converts an address to an EVM word.
#[inline]
pub fn address_to_word(address: Address) -> Word {
    address.into_word().into()
}

/// Converts a 256-bit hash to an EVM word.
#[inline]
pub const fn b256_to_word(value: B256) -> Word {
    Word::from_be_bytes(value.0)
}

/// Converts an EVM word to an address.
#[inline]
pub fn word_to_address(value: Word) -> Address {
    Address::from_word(B256::from(value.to_be_bytes::<32>()))
}

/// Converts an EVM word to `usize`, returning an invalid-operand OOG stop on overflow.
#[inline]
pub fn word_to_usize(value: Word) -> Result<usize> {
    value.try_into().map_err(|_| {
        cold_path();
        InstrStop::InvalidOperandOOG
    })
}

/// Converts an EVM word to `usize`, saturating on overflow.
#[inline]
pub fn word_to_usize_saturated(value: Word) -> usize {
    value.try_into().unwrap_or(usize::MAX)
}
