use super::utils::{as_usize, as_usize_saturated};
use crate::interpreter::{Word, memory::resize_memory};
use alloy_primitives::keccak256;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn keccak256_instr(cx: _, [offset, len]: [Word]) -> Result<out> {
    let len = as_usize(*len)?;
    let hash = if len == 0 {
        keccak256([])
    } else {
        let offset = as_usize(*offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
        keccak256(cx.state.memory.slice(offset, len)?)
    };
    *out = Word::from_be_bytes(hash.0);
}

#[instruction]
pub(in crate::interpreter) fn codesize(cx: _) -> out {
    *out = Word::from(cx.state.bytecode.len());
}

#[instruction]
pub(in crate::interpreter) fn codecopy(cx: _, [memory_offset, code_offset, len]: [Word]) -> Result {
    let len = as_usize(*len)?;
    if len == 0 {
        return Ok(());
    }
    let memory_offset = as_usize(*memory_offset)?;
    let code_offset = as_usize_saturated(*code_offset);
    resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, code_offset, len, cx.state.bytecode.as_slice())
}

#[instruction]
pub(in crate::interpreter) fn gas(cx: _) -> out {
    *out = Word::from(cx.gas.remaining());
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrStop, Word,
        instructions::tests::{push, run, run_stack},
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([]).0)]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(0x80));
        code.push(op::MSTORE8);
        push(&mut code, 0);
        push(&mut code, Word::from(1));
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([0x80]).0)]);

        let interpreter = run_stack([Word::MAX, Word::from(0)], op::KECCAK256);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([]).0)]);

        let interpreter = run_stack([Word::MAX, Word::from(1)], op::KECCAK256);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }

    #[test]
    fn codesize_opcode() {
        let interpreter = run([op::CODESIZE, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(2)]);

        let interpreter = run([op::PUSH1, 0x00, op::CODESIZE, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(0), Word::from(4)]);
    }

    #[test]
    fn codecopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(5));
        push(&mut code, Word::from(2));
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(code);
        let mut expected = [0u8; 32];
        expected[..2].copy_from_slice(&[op::CODECOPY, op::PUSH0]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(usize::MAX));
        push(&mut code, Word::from(1));
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run_stack([Word::MAX, Word::MAX, Word::from(0)], op::CODECOPY);
        assert!(matches!(interpreter.err, InstrStop::Stop));

        let interpreter = run_stack([Word::MAX, Word::from(0), Word::from(1)], op::CODECOPY);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }

    #[test]
    fn gas_opcode() {
        let interpreter = run([op::GAS, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 1);
        assert!(interpreter.stack()[0] < Word::from(10_000));

        let interpreter = run([op::GAS, op::GAS, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 2);
        assert!(interpreter.stack()[1] < interpreter.stack()[0]);
    }
}
