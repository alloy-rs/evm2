use crate::interpreter::{CtrlRef, Gas, InstructionCx, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, addr: &Word) -> out {
    *out = cx.state.host.balance(*addr);
}
