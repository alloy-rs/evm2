use crate::{
    interpreter::{Word, memory::resize_memory},
    utils::word_to_usize,
};
use evm2_macros::instruction;

#[instruction(needs_gas)]
pub(crate) fn mload(cx: _, [offset]: [Word]) -> Result<out> {
    let offset = word_to_usize(offset)?;
    resize_memory(cx.gas, cx.state.memory(), offset, 32)?;
    *out = cx.state.memory().get_word(offset);
}

#[instruction(needs_gas)]
pub(crate) fn mstore(cx: _, [offset, value]: [Word]) -> Result {
    let offset = word_to_usize(offset)?;
    resize_memory(cx.gas, cx.state.memory(), offset, 32)?;
    cx.state.memory().set(offset, &value.to_be_bytes::<32>());
}

#[instruction(needs_gas)]
pub(crate) fn mstore8(cx: _, [offset, value]: [Word]) -> Result {
    let offset = word_to_usize(offset)?;
    resize_memory(cx.gas, cx.state.memory(), offset, 1)?;
    cx.state.memory().set(offset, &[value.byte(0)]);
}

#[instruction]
pub(crate) fn msize(cx: _) -> out {
    *out = Word::from(cx.state.memory().len());
}

#[instruction(needs_gas)]
pub(crate) fn mcopy(cx: _, [dst, src, len]: [Word]) -> Result {
    let len = word_to_usize(len)?;
    cx.gas.spend(cx.state.gas_params().mcopy_cost(len))?;
    if len != 0 {
        let dst = word_to_usize(dst)?;
        let src = word_to_usize(src)?;
        resize_memory(cx.gas, cx.state.memory(), dst.max(src), len)?;
        cx.state.memory().copy(dst, src, len);
    };
}

#[cfg(test)]
mod tests {
    use crate::{
        SpecId,
        interpreter::{
            InstrStop, Word,
            instructions::tests::{RunConfig, push, run, run_stack},
            op,
        },
    };
    use alloc::vec::Vec;

    #[test]
    fn mload_opcode() {
        let value = Word::from(0xfeed);
        let mut code = Vec::new();
        push(&mut code, value);
        push(&mut code, 0);
        code.push(op::MSTORE);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let mut interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [value]);
        assert_eq!(interpreter.memory(30, 2), [0xfe, 0xed]);

        let interpreter = run_stack([Word::MAX], op::MLOAD);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn mstore_opcode() {
        let value = Word::from(0xfeed);
        let mut code = Vec::new();
        push(&mut code, value);
        push(&mut code, Word::from(8));
        code.push(op::MSTORE);
        code.push(op::MSIZE);
        code.push(op::STOP);

        let mut interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(64)]);
        assert_eq!(interpreter.memory(38, 2), [0xfe, 0xed]);

        let interpreter = run_stack([Word::MAX, Word::from(0)], op::MSTORE);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn mstore8_opcode() {
        let mut code = Vec::new();
        push(&mut code, Word::from(0x01ab));
        push(&mut code, Word::from(4));
        code.push(op::MSTORE8);
        push(&mut code, Word::from(4));
        code.push(op::MLOAD);
        code.push(op::STOP);

        let mut interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.memory(4, 1), [0xab]);
        assert_eq!(interpreter.stack()[0] >> 248, Word::from(0xab));

        let interpreter = run_stack([Word::MAX, Word::from(0)], op::MSTORE8);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn msize_opcode() {
        let interpreter = run(RunConfig::new([op::MSIZE, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(33));
        code.push(op::MSTORE);
        code.push(op::MSIZE);
        code.push(op::STOP);
        let interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(96)]);
    }

    #[test]
    fn mcopy_opcode() {
        let value = Word::from(0x1234);
        let mut code = Vec::new();
        push(&mut code, value);
        push(&mut code, 0);
        code.push(op::MSTORE);
        push(&mut code, Word::from(32));
        push(&mut code, 0);
        push(&mut code, Word::from(32));
        code.push(op::MCOPY);
        push(&mut code, Word::from(32));
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [value]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(1));
        push(&mut code, 0);
        code.push(op::MCOPY);
        code.push(op::MSIZE);
        code.push(op::STOP);
        let interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run_stack([Word::MAX, Word::MAX, Word::from(0)], op::MCOPY);
        core::assert_matches!(interpreter.err, InstrStop::Stop);

        let interpreter = run_stack([Word::MAX, Word::from(0), Word::from(1)], op::MCOPY);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn mcopy_charges_dynamic_gas() {
        let mut code = Vec::new();
        push(&mut code, Word::ZERO);
        push(&mut code, 0);
        code.push(op::MSTORE);
        push(&mut code, Word::from(32));
        push(&mut code, 0);
        push(&mut code, 0);
        code.extend([op::MCOPY, op::STOP]);

        let interpreter = run(RunConfig::new(code).spec(SpecId::CANCUN).gas_limit(26));

        core::assert_matches!(interpreter.err, InstrStop::OutOfGas);
    }
}
