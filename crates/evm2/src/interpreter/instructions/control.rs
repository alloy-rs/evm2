use super::utils::as_usize;
use crate::interpreter::{InstrErr, Word, memory::resize_memory};
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
pub(in crate::interpreter) fn jump(cx: _, [target]: [Word]) -> Result {
    let target = as_usize(*target)?;
    if !cx.state.bytecode.is_valid_jumpdest(target) {
        cold_path();
        return Err(InstrErr::Invalid);
    }
    unsafe { cx.pc.set_unchecked(target) };
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn jumpi(cx: _, [target, cond]: [Word]) -> Result {
    if !cond.is_zero() {
        let target = as_usize(*target)?;
        if !cx.state.bytecode.is_valid_jumpdest(target) {
            cold_path();
            return Err(InstrErr::Invalid);
        }
        unsafe { cx.pc.set_unchecked(target) };
    }
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn pc(cx: _) -> out {
    *out = Word::from(cx.pc.get() - 1);
}

#[instruction]
pub(in crate::interpreter) fn jumpdest() {}

#[instruction]
pub(in crate::interpreter) fn ret(cx: _, [offset, len]: [Word]) -> Result {
    let len = as_usize(*len)?;
    if len != 0 {
        let offset = as_usize(*offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
    }
    Err(InstrErr::Return)
}

#[instruction]
pub(in crate::interpreter) fn revert(cx: _, [offset, len]: [Word]) -> Result {
    let len = as_usize(*len)?;
    if len != 0 {
        let offset = as_usize(*offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
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

        let interpreter = run([op::STOP, op::INVALID]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 1);
    }

    #[test]
    fn invalid_opcode() {
        let interpreter = run([op::INVALID]);
        assert!(matches!(interpreter.err, InstrErr::Invalid));
        assert_eq!(interpreter.inner.pc, 1);

        let interpreter = run([0x0c]);
        assert!(matches!(interpreter.err, InstrErr::Invalid));
        assert_eq!(interpreter.inner.pc, 1);
    }

    #[test]
    fn jump_opcode() {
        let interpreter = run([op::PUSH1, 0x03, op::JUMP, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 5);

        let interpreter = run([op::PUSH1, 0x00, op::JUMP, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Invalid));

        let interpreter = run([op::PUSH1, 0x04, op::JUMP, op::STOP, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 6);
    }

    #[test]
    fn jumpi_opcode() {
        let interpreter =
            run([op::PUSH1, 0x06, op::PUSH1, 0x01, op::JUMPI, op::STOP, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 8);

        let interpreter =
            run([op::PUSH1, 0x06, op::PUSH1, 0x00, op::JUMPI, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 7);

        let interpreter = run([op::PUSH1, 0x05, op::PUSH1, 0x01, op::JUMPI, op::STOP, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Invalid));
    }

    #[test]
    fn pc_opcode() {
        let interpreter = run([op::PC, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run([op::JUMPDEST, op::PC, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.stack(), [Word::from(1)]);
    }

    #[test]
    fn jumpdest_opcode() {
        let interpreter = run([op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert!(interpreter.stack().is_empty());

        let interpreter = run([op::JUMPDEST, op::JUMPDEST, op::STOP]);
        assert!(matches!(interpreter.err, InstrErr::Stop));
        assert_eq!(interpreter.inner.pc, 3);
    }

    #[test]
    fn return_opcode() {
        let mut interpreter = run_stack([0, 0], op::RETURN);
        assert!(matches!(interpreter.err, InstrErr::Return));
        assert!(interpreter.memory(0, 0).is_empty());

        let mut interpreter = run_stack([0, 1], op::RETURN);
        assert!(matches!(interpreter.err, InstrErr::Return));
        assert_eq!(interpreter.memory(0, 1), [0]);
    }

    #[test]
    fn revert_opcode() {
        let mut interpreter = run_stack([0, 0], op::REVERT);
        assert!(matches!(interpreter.err, InstrErr::Revert));
        assert!(interpreter.memory(0, 0).is_empty());

        let mut interpreter = run_stack([2, 3], op::REVERT);
        assert!(matches!(interpreter.err, InstrErr::Revert));
        assert_eq!(interpreter.memory(2, 3), [0, 0, 0]);
    }
}
