use super::utils::as_usize;
use crate::interpreter::{
    CtrlRef, Gas, InstructionCx, Result, Stack, State, Word, memory::resize_memory,
};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn mload(cx: _, offset: &Word) -> Result<out> {
    let offset = as_usize(*offset)?;
    resize_memory(cx.gas, cx.state.memory, offset, 32)?;
    *out = cx.state.memory.get_word(offset)?;
}

#[instruction]
pub(in crate::interpreter) fn mstore(cx: _, offset: &Word, value: &Word) -> Result {
    let offset = as_usize(*offset)?;
    resize_memory(cx.gas, cx.state.memory, offset, 32)?;
    cx.state.memory.set(offset, &value.to_be_bytes::<32>())
}

#[instruction]
pub(in crate::interpreter) fn mstore8(cx: _, offset: &Word, value: &Word) -> Result {
    let offset = as_usize(*offset)?;
    resize_memory(cx.gas, cx.state.memory, offset, 1)?;
    cx.state.memory.set(offset, &[value.byte(0)])
}

#[instruction]
pub(in crate::interpreter) fn msize(cx: _) -> out {
    *out = Word::from(cx.state.memory.len());
}

#[instruction]
pub(in crate::interpreter) fn mcopy(cx: _, dst: &Word, src: &Word, len: &Word) -> Result {
    let len = as_usize(*len)?;
    if len == 0 {
        return Ok(());
    }
    let dst = as_usize(*dst)?;
    let src = as_usize(*src)?;
    resize_memory(cx.gas, cx.state.memory, dst.max(src), len)?;
    cx.state.memory.copy(dst, src, len)
}
