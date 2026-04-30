use super::{
    super::{CtrlRef, Gas, InstrErr, Result, Stack, State, Word},
    utils::as_usize,
};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn mload(offset: &Word) -> Result<out> {
    let offset = as_usize(*offset).ok_or(InstrErr::OutOfGas)?;
    *out = state.memory.get_word(offset)?;
}

#[instruction]
pub(in crate::interpreter) fn mstore(offset: &Word, value: &Word) -> Result {
    let offset = as_usize(*offset).ok_or(InstrErr::OutOfGas)?;
    state.memory.set(offset, &value.to_be_bytes::<32>())
}

#[instruction]
pub(in crate::interpreter) fn mstore8(offset: &Word, value: &Word) -> Result {
    let offset = as_usize(*offset).ok_or(InstrErr::OutOfGas)?;
    state.memory.set(offset, &[value.byte(0)])
}

#[instruction]
pub(in crate::interpreter) fn msize() -> Result<out> {
    *out = Word::from(state.memory.len());
}

#[instruction]
pub(in crate::interpreter) fn mcopy(dst: &Word, src: &Word, len: &Word) -> Result {
    let len = as_usize(*len).ok_or(InstrErr::OutOfGas)?;
    if len == 0 {
        return Ok(());
    }
    let dst = as_usize(*dst).ok_or(InstrErr::OutOfGas)?;
    let src = as_usize(*src).ok_or(InstrErr::OutOfGas)?;
    state.memory.copy(dst, src, len)
}
