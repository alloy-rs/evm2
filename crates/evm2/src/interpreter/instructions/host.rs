use crate::interpreter::{CtrlRef, Gas, InstructionCx, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, addr: &Word) -> out {
    *out = cx.state.host.balance(*addr);
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{InstrErr, Word, instructions::tests::run_stack, op};

    #[test]
    fn balance_opcode() {
        let interpreter = run_stack(&[Word::from(0xbeef)], op::BALANCE);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);
    }
}
