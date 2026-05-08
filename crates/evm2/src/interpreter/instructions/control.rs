use crate::{
    EvmTypes,
    interpreter::{
        InstrStop, Result, Word,
        memory::resize_memory,
        private::{GasInstructionCx, InstructionCx},
    },
    utils::{word_to_usize, word_to_usize_saturated},
};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub(crate) fn stop() -> Result {
    cold_path();
    Err(InstrStop::Stop)
}

#[instruction]
pub(crate) fn jump(cx: _, [target]: [Word]) -> Result {
    jump_inner(target, &mut cx)
}

#[instruction]
pub(crate) fn jumpi(cx: _, [target, cond]: [Word]) -> Result {
    if !cond.is_zero() {
        jump_inner(target, &mut cx)?;
    } else {
        unsafe { cx.pc.advance_unchecked(1) };
    };
}

#[inline(always)]
fn jump_inner<T: EvmTypes>(target: Word, cx: &mut InstructionCx<'_, '_, T>) -> Result {
    let target = word_to_usize_saturated(target);
    if !cx.state.bytecode().is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrStop::InvalidJump);
    }
    unsafe { cx.pc.set_unchecked(cx.state.bytecode(), target) };
    Ok(())
}

#[instruction]
pub(crate) fn pc(cx: _) -> out {
    *out = Word::from(cx.state.bytecode().pc_offset(*cx.pc));
}

#[instruction(dynamic_gas)]
pub(crate) fn gas(cx: _) -> out {
    *out = Word::from(cx.gas.remaining());
}

#[instruction]
pub(crate) fn jumpdest() {}

#[instruction(dynamic_gas)]
pub(crate) fn r#return(cx: _, [offset, len]: [Word]) -> Result {
    return_inner(cx, offset, len, InstrStop::Return)
}

#[instruction(dynamic_gas)]
pub(crate) fn revert(cx: _, [offset, len]: [Word]) -> Result {
    return_inner(cx, offset, len, InstrStop::Revert)
}

#[inline]
fn return_inner<T: EvmTypes>(
    cx: GasInstructionCx<'_, '_, T>,
    offset: Word,
    len: Word,
    result: InstrStop,
) -> Result {
    let len = word_to_usize(len)?;
    let offset = if len != 0 {
        let offset = word_to_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory(), offset, len)?;
        offset
    } else {
        0
    };
    let output = cx.state.memory().slice(offset, len) as *const [u8];
    cx.state.set_output(output);
    Err(result)
}

#[instruction]
pub(crate) fn invalid() -> Result {
    cold_path();
    Err(InstrStop::InvalidFEOpcode)
}

#[instruction]
pub(crate) fn unknown() -> Result {
    cold_path();
    Err(InstrStop::OpcodeNotFound)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrStop, Word,
        instructions::tests::{RunConfig, push, run, run_stack},
        op,
    };
    use alloc::vec::Vec;

    #[test]
    fn stop_opcode() {
        let interpreter = run(RunConfig::new([op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));

        let interpreter = run(RunConfig::new([op::STOP, op::INVALID]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
    }

    #[test]
    fn invalid_opcode() {
        let interpreter = run(RunConfig::new([op::INVALID]));
        assert!(matches!(interpreter.err, InstrStop::InvalidFEOpcode));

        let interpreter = run(RunConfig::new([0x0c]));
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));
    }

    #[test]
    fn jump_opcode() {
        let interpreter = run(RunConfig::new([op::PUSH1, 0x03, op::JUMP, op::JUMPDEST, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));

        let interpreter = run(RunConfig::new([op::PUSH1, 0x00, op::JUMP, op::JUMPDEST, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::InvalidJump));

        let mut code = Vec::new();
        push(&mut code, Word::MAX);
        code.push(op::JUMP);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::InvalidJump));

        let interpreter =
            run(RunConfig::new([op::PUSH1, 0x04, op::JUMP, op::STOP, op::JUMPDEST, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
    }

    #[test]
    fn jumpi_opcode() {
        let interpreter = run(RunConfig::new([
            op::PUSH1,
            0x01,
            op::PUSH1,
            0x06,
            op::JUMPI,
            op::STOP,
            op::JUMPDEST,
            op::STOP,
        ]));
        assert!(matches!(interpreter.err, InstrStop::Stop));

        let interpreter = run(RunConfig::new([
            op::PUSH1,
            0x00,
            op::PUSH1,
            0x06,
            op::JUMPI,
            op::JUMPDEST,
            op::STOP,
        ]));
        assert!(matches!(interpreter.err, InstrStop::Stop));

        let interpreter =
            run(RunConfig::new([op::PUSH1, 0x01, op::PUSH1, 0x05, op::JUMPI, op::STOP, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::InvalidJump));

        let mut code = Vec::new();
        push(&mut code, 1);
        push(&mut code, Word::MAX);
        code.push(op::JUMPI);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::InvalidJump));
    }

    #[test]
    fn pc_opcode() {
        let interpreter = run(RunConfig::new([op::PC, op::JUMPDEST, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run(RunConfig::new([op::JUMPDEST, op::PC, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(1)]);
    }

    #[test]
    fn gas_opcode() {
        let interpreter = run(RunConfig::new([op::GAS, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 1);
        assert!(interpreter.stack()[0] < Word::from(10_000));

        let interpreter = run(RunConfig::new([op::GAS, op::GAS, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 2);
        assert!(interpreter.stack()[1] < interpreter.stack()[0]);
    }

    #[test]
    fn jumpdest_opcode() {
        let interpreter = run(RunConfig::new([op::JUMPDEST, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert!(interpreter.stack().is_empty());

        let interpreter = run(RunConfig::new([op::JUMPDEST, op::JUMPDEST, op::STOP]));
        assert!(matches!(interpreter.err, InstrStop::Stop));
    }

    #[test]
    fn return_opcode() {
        let mut interpreter = run_stack([0, 0], op::RETURN);
        assert!(matches!(interpreter.err, InstrStop::Return));
        assert!(interpreter.memory(0, 0).is_empty());
        assert!(interpreter.output().is_empty());

        let mut interpreter = run_stack([0, 1], op::RETURN);
        assert!(matches!(interpreter.err, InstrStop::Return));
        assert_eq!(interpreter.memory(0, 1), [0]);
        assert_eq!(interpreter.output(), [0]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0xab));
        push(&mut code, Word::from(2));
        code.push(op::MSTORE8);
        push(&mut code, Word::from(3));
        push(&mut code, Word::from(2));
        code.push(op::RETURN);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::Return));
        assert_eq!(interpreter.output(), [0xab, 0, 0]);

        let interpreter = run_stack([Word::from(0), Word::MAX], op::RETURN);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }

    #[test]
    fn revert_opcode() {
        let mut interpreter = run_stack([0, 0], op::REVERT);
        assert!(matches!(interpreter.err, InstrStop::Revert));
        assert!(interpreter.memory(0, 0).is_empty());
        assert!(interpreter.output().is_empty());

        let mut interpreter = run_stack([2, 3], op::REVERT);
        assert!(matches!(interpreter.err, InstrStop::Revert));
        assert_eq!(interpreter.memory(2, 3), [0, 0, 0]);
        assert_eq!(interpreter.output(), [0, 0, 0]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0xcd));
        push(&mut code, Word::from(4));
        code.push(op::MSTORE8);
        push(&mut code, Word::from(2));
        push(&mut code, Word::from(4));
        code.push(op::REVERT);
        let interpreter = run(RunConfig::new(code));
        assert!(matches!(interpreter.err, InstrStop::Revert));
        assert_eq!(interpreter.output(), [0xcd, 0]);

        let interpreter = run_stack([Word::from(0), Word::MAX], op::REVERT);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }
}
