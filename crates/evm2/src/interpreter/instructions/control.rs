use super::{
    super::{CtrlRef, Gas, InstrErr, Result, Stack, State, Word},
    utils::as_usize,
};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction(raw)]
pub(in crate::interpreter) fn stop() -> Result {
    cold_path();
    return Err(InstrErr::Stop);
}

#[instruction(raw)]
pub(in crate::interpreter) fn invalid() -> Result {
    cold_path();
    return Err(InstrErr::Invalid);
}

#[instruction(raw)]
pub(in crate::interpreter) fn jump() -> Result {
    let target = stack.pop().and_then(|target| as_usize(target).ok_or(InstrErr::Invalid))?;
    if !ctrl.is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrErr::Invalid);
    }
    unsafe { ctrl.set_unchecked(target) };
    return Ok(());
}

#[instruction(raw)]
pub(in crate::interpreter) fn jumpi() -> Result {
    let [target, cond] = stack.popn()?;
    if !cond.is_zero() {
        let target = as_usize(target).ok_or(InstrErr::Invalid)?;
        if !ctrl.is_valid_jumpdest(target) {
            cold_path();
            return Err(InstrErr::Invalid);
        }
        unsafe { ctrl.set_unchecked(target) };
    }
    return Ok(());
}

#[instruction(raw)]
pub(in crate::interpreter) fn pc() -> Result {
    return stack.push(Word::from(ctrl.pc() - 1));
}

#[instruction(raw)]
pub(in crate::interpreter) fn jumpdest() -> Result {
    return Ok(());
}
