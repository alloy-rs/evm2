use super::utils::as_usize;
use crate::interpreter::{CtrlRef, Gas, InstructionCx, Result, Stack, State, Word};
use alloy_primitives::keccak256;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn keccak256_instr(cx: _, offset: &Word, len: &Word) -> Result<out> {
    let offset = as_usize(*offset)?;
    let len = as_usize(*len)?;
    crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, offset, len)?;
    let hash = keccak256(cx.state.memory.slice(offset, len)?);
    *out = Word::from_be_bytes(hash.0);
}

#[instruction]
pub(in crate::interpreter) fn codesize(cx: _) -> out {
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
    crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, code_offset, len, cx.ctrl.as_slice())
}

#[instruction]
pub(in crate::interpreter) fn gas(cx: _) -> out {
    *out = Word::from(cx.gas.remaining());
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrErr, Word,
        instructions::tests::{push, run},
        op,
    };
    use alloc::vec::Vec;
    use alloy_primitives::keccak256;

    #[test]
    fn system_opcodes() {
        let interpreter = run([op::CODESIZE, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from(2)]);

        let interpreter = run([op::GAS, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack().len(), 1);
        assert!(interpreter.stack()[0] < Word::from(10_000));

        let mut code = Vec::new();
        push(&mut code, Word::ZERO);
        push(&mut code, Word::ZERO);
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([]).0)]);
    }

    #[test]
    fn codecopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, Word::ZERO);
        push(&mut code, Word::from(5));
        push(&mut code, Word::from(2));
        code.push(op::CODECOPY);
        push(&mut code, Word::ZERO);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(code);
        let mut expected = [0u8; 32];
        expected[..2].copy_from_slice(&[op::CODECOPY, op::PUSH0]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);
    }
}
