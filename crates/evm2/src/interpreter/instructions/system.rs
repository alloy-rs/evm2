use super::{
    super::{CtrlRef, Gas, InstrErr, Result, Stack, State, Word},
    utils::as_usize,
};
use alloy_primitives::keccak256;
use evm2_macros::instruction;

#[instruction(raw)]
pub(in crate::interpreter) fn keccak256_instr() -> Result {
    let [offset, len] = stack.popn()?;
    let offset = as_usize(offset).ok_or(InstrErr::OutOfGas)?;
    let len = as_usize(len).ok_or(InstrErr::OutOfGas)?;
    let hash = keccak256(state.memory.slice(offset, len)?);
    return stack.push(Word::from_be_bytes(hash.0));
}

#[instruction(raw)]
pub(in crate::interpreter) fn codesize() -> Result {
    return stack.push(Word::from(ctrl.len()));
}

#[instruction(raw)]
pub(in crate::interpreter) fn codecopy() -> Result {
    let [memory_offset, code_offset, len] = stack.popn()?;
    let memory_offset = as_usize(memory_offset).ok_or(InstrErr::OutOfGas)?;
    let code_offset = as_usize(code_offset).unwrap_or(usize::MAX);
    let len = as_usize(len).ok_or(InstrErr::OutOfGas)?;
    if len == 0 {
        return Ok(());
    }

    let mut remaining = len;
    let mut dst = memory_offset;
    if code_offset < ctrl.len() {
        let available = (ctrl.len() - code_offset).min(len);
        let bytes = unsafe { ctrl.code_slice_unchecked(code_offset, available) };
        state.memory.set(dst, bytes)?;
        remaining -= available;
        dst += available;
    }
    if remaining != 0 {
        state.memory.set(dst, &alloc::vec![0; remaining])?;
    }
    return Ok(());
}

#[instruction(raw)]
pub(in crate::interpreter) fn gas_instr() -> Result {
    return stack.push(Word::from(gas.remaining));
}
