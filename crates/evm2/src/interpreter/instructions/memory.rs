use super::{
    super::{CtrlRef, Gas, InstrErr, Result, Stack, State, Word},
    utils::as_usize,
};
use evm2_macros::instruction;

#[instruction(raw)]
pub(in crate::interpreter) fn mload() -> Result {
    let slot = unsafe { stack.top_unchecked() };
    let offset = as_usize(*slot).ok_or(InstrErr::OutOfGas)?;
    *slot = state.memory.get_word(offset)?;
    return Ok(());
}

#[instruction(raw)]
pub(in crate::interpreter) fn mstore() -> Result {
    let [offset, value] = stack.popn()?;
    let offset = as_usize(offset).ok_or(InstrErr::OutOfGas)?;
    return state.memory.set(offset, &value.to_be_bytes::<32>());
}

#[instruction(raw)]
pub(in crate::interpreter) fn mstore8() -> Result {
    let [offset, value] = stack.popn()?;
    let offset = as_usize(offset).ok_or(InstrErr::OutOfGas)?;
    return state.memory.set(offset, &[value.byte(0)]);
}

#[instruction(raw)]
pub(in crate::interpreter) fn msize() -> Result {
    return stack.push(Word::from(state.memory.len()));
}

#[instruction(raw)]
pub(in crate::interpreter) fn mcopy() -> Result {
    let [dst, src, len] = stack.popn()?;
    let len = as_usize(len).ok_or(InstrErr::OutOfGas)?;
    if len == 0 {
        return Ok(());
    }
    let dst = as_usize(dst).ok_or(InstrErr::OutOfGas)?;
    let src = as_usize(src).ok_or(InstrErr::OutOfGas)?;
    return state.memory.copy(dst, src, len);
}
