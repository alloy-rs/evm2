use crate::interpreter::{CtrlRef, Gas, InstructionCx, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, addr: &Word) -> out {
    *out = cx.state.host.balance(*addr);
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{Word, instructions::tests::assert_stack, op};

    #[test]
    fn balance_opcode() {
        assert_stack(&[Word::from(0xbeef)], op::BALANCE, &[Word::from(0xbeef)]);
        assert_stack(&[Word::ZERO], op::BALANCE, &[Word::ZERO]);
        assert_stack(&[Word::MAX], op::BALANCE, &[Word::MAX]);
    }
}
