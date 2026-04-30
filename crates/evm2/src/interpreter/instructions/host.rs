use crate::interpreter::{Bytecode, Gas, InstructionCx, Pc, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, [addr]: [Word]) -> out {
    *out = cx.state.host.balance(*addr);
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{Word, instructions::tests::assert_stack};

    fn neg(value: u64) -> Word {
        Word::from(0).wrapping_sub(Word::from(value))
    }

    #[test]
    fn balance_opcode() {
        assert_stack!(BALANCE(0xbeef), 0xbeef);
        assert_stack!(BALANCE(0), 0);
        assert_stack!(BALANCE(neg(1)), neg(1));
    }
}
