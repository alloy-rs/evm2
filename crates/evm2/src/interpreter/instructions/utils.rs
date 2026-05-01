use crate::interpreter::{InstrStop, Result, SpecId, Word};
use alloy_primitives::{Address, B256};
use core::hint::cold_path;

#[inline]
pub(in crate::interpreter) fn address_to_word(address: Address) -> Word {
    address.into_word().into()
}

#[inline]
pub(in crate::interpreter) const fn b256_to_word(value: B256) -> Word {
    Word::from_be_bytes(value.0)
}

#[inline]
pub(in crate::interpreter) fn word_to_address(value: Word) -> Address {
    Address::from_word(B256::from(value.to_be_bytes::<32>()))
}

#[inline]
pub(in crate::interpreter) fn as_usize(value: Word) -> Result<usize> {
    value.try_into().map_err(|_| {
        cold_path();
        InstrStop::InvalidOperandOOG
    })
}

#[inline]
pub(in crate::interpreter) fn as_usize_saturated(value: Word) -> usize {
    value.try_into().unwrap_or(usize::MAX)
}

#[inline]
pub(in crate::interpreter) const fn check_spec(spec: SpecId, min: SpecId) -> Result {
    if !spec.enables(min) {
        cold_path();
        return Err(InstrStop::NotActivated);
    }
    Ok(())
}
