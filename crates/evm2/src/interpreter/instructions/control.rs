use super::utils::{as_usize, as_usize_saturated};
use crate::interpreter::{Host, InstrStop, PcMut, Result, State, Word, memory::resize_memory};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn stop() -> Result {
    cold_path();
    Err(InstrStop::Stop)
}

#[instruction]
pub(in crate::interpreter) fn jump(cx: _, [target]: [Word]) -> Result {
    jump_inner(target, cx.pc, cx.state)
}

#[instruction]
pub(in crate::interpreter) fn jumpi(cx: _, [target, cond]: [Word]) -> Result {
    if !cond.is_zero() {
        jump_inner(target, cx.pc, cx.state)?;
    }
    Ok(())
}

#[inline(always)]
fn jump_inner<H: Host + ?Sized>(
    target: Word,
    mut pc_mut: PcMut<'_>,
    state: &State<'_, H>,
) -> Result {
    let target = as_usize_saturated(target);
    if !state.bytecode.is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrStop::InvalidJump);
    }
    unsafe { pc_mut.set_unchecked(target) };
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn pc(cx: _) -> out {
    *out = Word::from(cx.pc.get() - 1);
}

#[instruction]
pub(in crate::interpreter) fn gas(cx: _) -> out {
    *out = Word::from(cx.gas.remaining());
}

#[instruction]
pub(in crate::interpreter) fn jumpdest() {}

#[instruction]
pub(in crate::interpreter) fn r#return(cx: _, [offset, len]: [Word]) -> Result {
    let len = as_usize(len)?;
    if len != 0 {
        let offset = as_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
    }
    Err(InstrStop::Return)
}

#[instruction]
pub(in crate::interpreter) fn revert(cx: _, [offset, len]: [Word]) -> Result {
    let len = as_usize(len)?;
    if len != 0 {
        let offset = as_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
    }
    Err(InstrStop::Revert)
}

#[instruction]
pub(in crate::interpreter) fn invalid() -> Result {
    cold_path();
    Err(InstrStop::InvalidFEOpcode)
}

#[instruction]
pub(in crate::interpreter) fn unknown() -> Result {
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
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 1);

        let interpreter = run(RunConfig::new([op::STOP, op::INVALID]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 1);
    }

    #[test]
    fn invalid_opcode() {
        let interpreter = run(RunConfig::new([op::INVALID]));
        core::assert_matches!(interpreter.err, InstrStop::InvalidFEOpcode);
        assert_eq!(interpreter.pc, 1);

        let interpreter = run(RunConfig::new([0x0c]));
        core::assert_matches!(interpreter.err, InstrStop::OpcodeNotFound);
        assert_eq!(interpreter.pc, 1);
    }

    #[test]
    fn jump_opcode() {
        let interpreter = run(RunConfig::new([op::PUSH1, 0x03, op::JUMP, op::JUMPDEST, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 5);

        let interpreter = run(RunConfig::new([op::PUSH1, 0x00, op::JUMP, op::JUMPDEST, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::InvalidJump);

        let mut code = Vec::new();
        push(&mut code, Word::MAX);
        code.push(op::JUMP);
        let interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::InvalidJump);

        let interpreter =
            run(RunConfig::new([op::PUSH1, 0x04, op::JUMP, op::STOP, op::JUMPDEST, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 6);
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
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 8);

        let interpreter = run(RunConfig::new([
            op::PUSH1,
            0x00,
            op::PUSH1,
            0x06,
            op::JUMPI,
            op::JUMPDEST,
            op::STOP,
        ]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 7);

        let interpreter =
            run(RunConfig::new([op::PUSH1, 0x01, op::PUSH1, 0x05, op::JUMPI, op::STOP, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::InvalidJump);

        let mut code = Vec::new();
        push(&mut code, 1);
        push(&mut code, Word::MAX);
        code.push(op::JUMPI);
        let interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::InvalidJump);
    }

    #[test]
    fn pc_opcode() {
        let interpreter = run(RunConfig::new([op::PC, op::JUMPDEST, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run(RunConfig::new([op::JUMPDEST, op::PC, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(1)]);
    }

    #[test]
    fn gas_opcode() {
        let interpreter = run(RunConfig::new([op::GAS, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack().len(), 1);
        assert!(interpreter.stack()[0] < Word::from(10_000));

        let interpreter = run(RunConfig::new([op::GAS, op::GAS, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack().len(), 2);
        assert!(interpreter.stack()[1] < interpreter.stack()[0]);
    }

    #[test]
    fn jumpdest_opcode() {
        let interpreter = run(RunConfig::new([op::JUMPDEST, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert!(interpreter.stack().is_empty());

        let interpreter = run(RunConfig::new([op::JUMPDEST, op::JUMPDEST, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.pc, 3);
    }

    #[test]
    fn return_opcode() {
        let mut interpreter = run_stack([0, 0], op::RETURN);
        core::assert_matches!(interpreter.err, InstrStop::Return);
        assert!(interpreter.memory(0, 0).is_empty());

        let mut interpreter = run_stack([0, 1], op::RETURN);
        core::assert_matches!(interpreter.err, InstrStop::Return);
        assert_eq!(interpreter.memory(0, 1), [0]);

        let interpreter = run_stack([Word::from(0), Word::MAX], op::RETURN);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn revert_opcode() {
        let mut interpreter = run_stack([0, 0], op::REVERT);
        core::assert_matches!(interpreter.err, InstrStop::Revert);
        assert!(interpreter.memory(0, 0).is_empty());

        let mut interpreter = run_stack([2, 3], op::REVERT);
        core::assert_matches!(interpreter.err, InstrStop::Revert);
        assert_eq!(interpreter.memory(2, 3), [0, 0, 0]);

        let interpreter = run_stack([Word::from(0), Word::MAX], op::REVERT);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }
}
