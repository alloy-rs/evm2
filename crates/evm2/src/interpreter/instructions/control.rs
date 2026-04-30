use super::{
    super::{CtrlRef, Gas, InstrErr, Result, Stack, State, Word},
    utils::as_usize,
};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn stop() -> Result {
    cold_path();
    return Err(InstrErr::Stop);
}

#[instruction]
pub(in crate::interpreter) fn invalid() -> Result {
    cold_path();
    return Err(InstrErr::Invalid);
}

#[instruction]
pub(in crate::interpreter) fn jump(target: &Word) -> Result {
    let target = as_usize(*target).ok_or(InstrErr::Invalid)?;
    if !ctrl.is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrErr::Invalid);
    }
    unsafe { ctrl.set_unchecked(target) };
    return Ok(());
}

#[instruction]
pub(in crate::interpreter) fn jumpi(target: &Word, cond: &Word) -> Result {
    if !cond.is_zero() {
        let target = as_usize(*target).ok_or(InstrErr::Invalid)?;
        if !ctrl.is_valid_jumpdest(target) {
            cold_path();
            return Err(InstrErr::Invalid);
        }
        unsafe { ctrl.set_unchecked(target) };
    }
    return Ok(());
}

#[instruction]
pub(in crate::interpreter) fn pc() -> Result<out> {
    *out = Word::from(ctrl.pc() - 1);
}

#[instruction]
pub(in crate::interpreter) fn jumpdest() -> Result {
    return Ok(());
}

#[instruction]
pub(in crate::interpreter) fn ret(offset: &Word, len: &Word) -> Result {
    let len = as_usize(*len).ok_or(InstrErr::OutOfGas)?;
    if len != 0 {
        let offset = as_usize(*offset).ok_or(InstrErr::OutOfGas)?;
        state.memory.resize(offset, len)?;
    }
    return Err(InstrErr::Return);
}

#[instruction]
pub(in crate::interpreter) fn revert(offset: &Word, len: &Word) -> Result {
    let len = as_usize(*len).ok_or(InstrErr::OutOfGas)?;
    if len != 0 {
        let offset = as_usize(*offset).ok_or(InstrErr::OutOfGas)?;
        state.memory.resize(offset, len)?;
    }
    return Err(InstrErr::Revert);
}
