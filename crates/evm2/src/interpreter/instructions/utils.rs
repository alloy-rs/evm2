use core::hint::cold_path;

use crate::interpreter::{InstrErr, Result, Word};

#[inline]
pub(in crate::interpreter) fn as_usize(value: Word) -> Result<usize> {
    value.try_into().map_err(|_| {
        cold_path();
        InstrErr::OutOfGas
    })
}
