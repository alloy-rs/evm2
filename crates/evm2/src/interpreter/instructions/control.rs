use super::utils::as_usize;
use crate::interpreter::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn stop() -> Result {
    cold_path();
    Err(InstrErr::Stop)
}

#[instruction]
pub(in crate::interpreter) fn invalid() -> Result {
    cold_path();
    Err(InstrErr::Invalid)
}

#[instruction]
pub(in crate::interpreter) fn jump(cx: _, target: &Word) -> Result {
    let target = as_usize(*target)?;
    if !cx.ctrl.is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrErr::Invalid);
    }
    unsafe { cx.ctrl.set_unchecked(target) };
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn jumpi(cx: _, target: &Word, cond: &Word) -> Result {
    if !cond.is_zero() {
        let target = as_usize(*target)?;
        if !cx.ctrl.is_valid_jumpdest(target) {
            cold_path();
            return Err(InstrErr::Invalid);
        }
        unsafe { cx.ctrl.set_unchecked(target) };
    }
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn pc(cx: _) -> Result<out> {
    *out = Word::from(cx.ctrl.pc() - 1);
}

#[instruction]
pub(in crate::interpreter) fn jumpdest() -> Result {
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn ret(cx: _, offset: &Word, len: &Word) -> Result {
    let len = as_usize(*len)?;
    if len != 0 {
        let offset = as_usize(*offset)?;
        cx.state.memory.resize(offset, len)?;
    }
    Err(InstrErr::Return)
}

#[instruction]
pub(in crate::interpreter) fn revert(cx: _, offset: &Word, len: &Word) -> Result {
    let len = as_usize(*len)?;
    if len != 0 {
        let offset = as_usize(*offset)?;
        cx.state.memory.resize(offset, len)?;
    }
    Err(InstrErr::Revert)
}
