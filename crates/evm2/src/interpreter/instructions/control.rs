use super::utils::as_usize;
use crate::interpreter::{CtrlRef, Gas, InstrErr, InstructionCx, Result, Stack, State, Word};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn stop() -> Result {
    cold_path();
    Err(InstrErr::Stop)
}

#[instruction]
pub(in crate::interpreter) fn invalid() -> Result {
    cold_path();
    Err(InstrErr::Invalid)
}

#[instruction]
pub(in crate::interpreter) fn jump(cx: _, target: &Word) -> Result {
    let target = as_usize(*target)?;
    if !cx.ctrl.is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrErr::Invalid);
    }
    unsafe { cx.ctrl.set_unchecked(target) };
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn jumpi(cx: _, target: &Word, cond: &Word) -> Result {
    if !cond.is_zero() {
        let target = as_usize(*target)?;
        if !cx.ctrl.is_valid_jumpdest(target) {
            cold_path();
            return Err(InstrErr::Invalid);
        }
        unsafe { cx.ctrl.set_unchecked(target) };
    }
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn pc(cx: _) -> out {
    *out = Word::from(cx.ctrl.pc() - 1);
}

#[instruction]
pub(in crate::interpreter) fn jumpdest() {}

#[instruction]
pub(in crate::interpreter) fn ret(cx: _, offset: &Word, len: &Word) -> Result {
    let len = as_usize(*len)?;
    if len != 0 {
        let offset = as_usize(*offset)?;
        crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, offset, len)?;
        cx.state.memory.resize(offset, len)?;
    }
    Err(InstrErr::Return)
}

#[instruction]
pub(in crate::interpreter) fn revert(cx: _, offset: &Word, len: &Word) -> Result {
    let len = as_usize(*len)?;
    if len != 0 {
        let offset = as_usize(*offset)?;
        crate::interpreter::memory::resize_memory(cx.gas, cx.state.memory, offset, len)?;
        cx.state.memory.resize(offset, len)?;
    }
    Err(InstrErr::Revert)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrErr, Word,
        instructions::tests::{run, run_stack},
        op,
    };

    #[test]
    fn stop_opcode() {
        let interpreter = run([op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 1);
    }

    #[test]
    fn invalid_opcode() {
        let interpreter = run([op::INVALID]);
        assert!(matches!(interpreter.err, InstrErr::Invalid));
        assert_eq!(interpreter.inner.pc, 1);
    }

    #[test]
    fn jump_opcodes() {
        let interpreter = run([op::PUSH1, 0x03, op::JUMP, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 5);

        let interpreter =
            run([op::PUSH1, 0x06, op::PUSH1, 0x00, op::JUMPI, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 7);

        let interpreter =
            run([op::PUSH1, 0x06, op::PUSH1, 0x01, op::JUMPI, op::STOP, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 8);
    }

    #[test]
    fn pc_and_jumpdest_opcodes() {
        let interpreter = run([op::PC, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::ZERO]);
    }

    #[test]
    fn return_opcodes() {
        let mut interpreter = run_stack(&[Word::ZERO, Word::ZERO], op::RETURN);
        assert!(matches!(interpreter.err, InstrErr::Return));
        assert!(interpreter.memory(0, 0).is_empty());

        let mut interpreter = run_stack(&[Word::ZERO, Word::ZERO], op::REVERT);
        assert!(matches!(interpreter.err, InstrErr::Revert));
        assert!(interpreter.memory(0, 0).is_empty());
    }
}
