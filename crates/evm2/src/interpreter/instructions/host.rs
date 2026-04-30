use crate::interpreter::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, addr: &Word) -> Result<out> {
    *out = cx.host.balance(*addr);
}
