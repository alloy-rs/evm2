use crate::{
    interpreter::{Word, memory::resize_memory},
    utils::{b256_to_word, word_to_usize},
};
use alloy_primitives::keccak256 as keccak256_hash;
use evm2_macros::instruction;

#[instruction(dynamic_gas)]
pub(crate) fn keccak256(cx: _, [offset, len]: [Word]) -> Result<out> {
    let len = word_to_usize(len)?;
    cx.gas.spend(cx.state.gas_params().keccak256_word_cost(len))?;
    let hash = if len == 0 {
        keccak256_hash([])
    } else {
        let offset = word_to_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory(), offset, len)?;
        keccak256_hash(cx.state.memory().slice(offset, len))
    };
    *out = b256_to_word(hash);
}

#[cfg(test)]
mod tests {
    use crate::{
        interpreter::{
            InstrStop, Word,
            instructions::tests::{RunConfig, push, run, run_stack},
            opcode,
        },
        utils::b256_to_word,
    };
    use alloc::vec::Vec;
    use alloy_primitives::keccak256;

    #[test]
    fn keccak256_opcode() {
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, 0);
        code.push(opcode::KECCAK256);
        code.push(opcode::STOP);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [b256_to_word(keccak256([]))]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0x80));
        push(&mut code, 0);
        code.push(opcode::MSTORE8);
        push(&mut code, Word::from(1));
        push(&mut code, 0);
        code.push(opcode::KECCAK256);
        code.push(opcode::STOP);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [b256_to_word(keccak256([0x80]))]);

        let interpreter = run_stack([Word::MAX, Word::from(0)], opcode::KECCAK256);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [b256_to_word(keccak256([]))]);

        let interpreter = run_stack([Word::MAX, Word::from(1)], opcode::KECCAK256);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }
}
