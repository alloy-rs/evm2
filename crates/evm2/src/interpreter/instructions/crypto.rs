use super::utils::{as_usize, b256_to_word};
use crate::interpreter::{Word, memory::resize_memory};
use alloy_primitives::keccak256 as keccak256_hash;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn keccak256(cx: _, [offset, len]: [Word]) -> Result<out> {
    let len = as_usize(len)?;
    let hash = if len == 0 {
        keccak256_hash([])
    } else {
        let offset = as_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
        keccak256_hash(cx.state.memory.slice(offset, len)?)
    };
    *out = b256_to_word(hash);
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrStop, Word,
        instructions::{
            tests::{push, run, run_stack},
            utils::b256_to_word,
        },
        op,
    };
    use alloc::vec::Vec;
    use alloy_primitives::keccak256;

    #[test]
    fn keccak256_opcode() {
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, 0);
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interpreter = run(code);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(keccak256([]))]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0x80));
        push(&mut code, 0);
        code.push(op::MSTORE8);
        push(&mut code, Word::from(1));
        push(&mut code, 0);
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interpreter = run(code);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(keccak256([0x80]))]);

        let interpreter = run_stack([Word::MAX, Word::from(0)], op::KECCAK256);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(keccak256([]))]);

        let interpreter = run_stack([Word::MAX, Word::from(1)], op::KECCAK256);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }
}
