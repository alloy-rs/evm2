use crate::interpreter::{InstrStop, Result, Word};
use core::hint::cold_path;

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
