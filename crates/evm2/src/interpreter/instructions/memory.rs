use super::utils::as_usize;
use crate::interpreter::{CtrlRef, Gas, InstructionCx, Result, Stack, State, Word};
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn mload(cx: _, [offset]: [Word]) -> Result<out> {
    let offset = as_usize(*offset)?;
    crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, offset, 32)?;
    *out = cx.state.memory.get_word(offset)?;
}

#[instruction]
pub(in crate::interpreter) fn mstore(cx: _, [offset, value]: [Word]) -> Result {
    let offset = as_usize(*offset)?;
    crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, offset, 32)?;
    cx.state.memory.set(offset, &value.to_be_bytes::<32>())
}

#[instruction]
pub(in crate::interpreter) fn mstore8(cx: _, [offset, value]: [Word]) -> Result {
    let offset = as_usize(*offset)?;
    crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, offset, 1)?;
    cx.state.memory.set(offset, &[value.byte(0)])
}

#[instruction]
pub(in crate::interpreter) fn msize(cx: _) -> out {
    *out = Word::from(cx.state.memory.len());
}

#[instruction]
pub(in crate::interpreter) fn mcopy(cx: _, [dst, src, len]: [Word]) -> Result {
    let len = as_usize(*len)?;
    if len == 0 {
        return Ok(());
    }
    let dst = as_usize(*dst)?;
    let src = as_usize(*src)?;
    crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, dst.max(src), len)?;
    cx.state.memory.copy(dst, src, len)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrErr, Word,
        instructions::tests::{push, run},
        op,
    };
    use alloc::vec::Vec;

    #[test]
    fn mload_opcode() {
        let value = Word::from(0xfeed);
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, value);
        code.push(op::MSTORE);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let mut interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [value]);
        assert_eq!(interpreter.memory(30, 2), [0xfe, 0xed]);
    }

    #[test]
    fn mstore_opcode() {
        let value = Word::from(0xfeed);
        let mut code = Vec::new();
        push(&mut code, Word::from(8));
        push(&mut code, value);
        code.push(op::MSTORE);
        code.push(op::MSIZE);
        code.push(op::STOP);

        let mut interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from(64)]);
        assert_eq!(interpreter.memory(38, 2), [0xfe, 0xed]);
    }

    #[test]
    fn mstore8_opcode() {
        let mut code = Vec::new();
        push(&mut code, Word::from(4));
        push(&mut code, Word::from(0x01ab));
        code.push(op::MSTORE8);
        push(&mut code, Word::from(4));
        code.push(op::MLOAD);
        code.push(op::STOP);

        let mut interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.memory(4, 1), [0xab]);
        assert_eq!(interpreter.stack()[0] >> 248, Word::from(0xab));
    }

    #[test]
    fn msize_opcode() {
        let interpreter = run([op::MSIZE, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [0]);

        let mut code = Vec::new();
        push(&mut code, Word::from(33));
        push(&mut code, 0);
        code.push(op::MSTORE);
        code.push(op::MSIZE);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from(96)]);
    }

    #[test]
    fn mcopy_opcode() {
        let value = Word::from(0x1234);
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, value);
        code.push(op::MSTORE);
        push(&mut code, Word::from(32));
        push(&mut code, 0);
        push(&mut code, Word::from(32));
        code.push(op::MCOPY);
        push(&mut code, Word::from(32));
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [value]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(1));
        push(&mut code, 0);
        code.push(op::MCOPY);
        code.push(op::MSIZE);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [0]);
    }
}
