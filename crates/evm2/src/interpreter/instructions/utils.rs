use crate::interpreter::{InstrStop, Result, Word};
use core::hint::cold_path;

#[inline]
pub(in crate::interpreter) fn as_usize(value: Word) -> Result<usize> {
    value.try_into().map_err(|_| {
        cold_path();
        InstrStop::OutOfGas
    })
}
