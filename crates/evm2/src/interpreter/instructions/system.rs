use super::utils::as_usize;
use crate::interpreter::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
use alloy_primitives::keccak256;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn keccak256_instr(cx: _, offset: &Word, len: &Word) -> Result<out> {
    let offset = as_usize(*offset)?;
    let len = as_usize(*len)?;
    let hash = keccak256(cx.state.memory.slice(offset, len)?);
    *out = Word::from_be_bytes(hash.0);
}

#[instruction]
pub(in crate::interpreter) fn codesize(cx: _) -> Result<out> {
    *out = Word::from(cx.ctrl.len());
}

#[instruction]
pub(in crate::interpreter) fn codecopy(
    cx: _,
    memory_offset: &Word,
    code_offset: &Word,
    len: &Word,
) -> Result {
    let memory_offset = as_usize(*memory_offset)?;
    let code_offset = as_usize(*code_offset).unwrap_or(usize::MAX);
    let len = as_usize(*len)?;
    if len == 0 {
        return Ok(());
    }

    let mut remaining = len;
    let mut dst = memory_offset;
    if code_offset < cx.ctrl.len() {
        let available = (cx.ctrl.len() - code_offset).min(len);
        let bytes = unsafe { cx.ctrl.code_slice_unchecked(code_offset, available) };
        cx.state.memory.set(dst, bytes)?;
        remaining -= available;
        dst += available;
    }
    if remaining != 0 {
        cx.state.memory.set(dst, &alloc::vec![0; remaining])?;
    }
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn gas_instr(cx: _) -> Result<out> {
    *out = Word::from(cx.gas.remaining);
}
