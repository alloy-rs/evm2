use crate::{
    EvmTypesHost,
    bytecode::chunks::{CODE_CHUNK_SIZE, code_chunk_gas},
    interpreter::{Host, InstrStop, Result, Word, op, opcode::OpCode, private::GasInstructionCx},
    utils::{word_to_usize, word_to_usize_saturated},
};
use core::hint::cold_path;
use evm2_macros::instruction;

#[instruction]
pub fn stop() -> Result {
    cold_path();
    Err(InstrStop::Stop)
}

#[instruction(dynamic_gas)]
pub fn jump(cx: _, [target]: [Word]) -> Result {
    jump_inner(*target, &mut cx)
}

#[instruction(dynamic_gas)]
pub fn jumpi(cx: _, [target, cond]: [Word]) -> Result {
    if !cond.is_zero() {
        jump_inner(*target, &mut cx)?;
    } else {
        unsafe { cx.pc.advance_unchecked(1) };
    };
}

#[inline(always)]
fn jump_inner<T: EvmTypesHost>(target: Word, cx: &mut GasInstructionCx<'_, '_, '_, T>) -> Result {
    let target = word_to_usize_saturated(target);
    let local_target = if cx.state.is_chunked_code() {
        if target >= cx.state.code_size() {
            cold_path();
            return Err(InstrStop::InvalidJump);
        }
        let chunk_index = (target / CODE_CHUNK_SIZE) as u32;
        let local_target = target % CODE_CHUNK_SIZE;
        if chunk_index != cx.state.code_chunk_index() {
            let cold_cost = code_chunk_gas(1).unwrap();
            let skip_cold_load = cx.gas.remaining() < cold_cost;
            let address = cx.state.message().code_address;
            let load = cx
                .state
                .host()
                .load_code_chunk(&address, chunk_index, skip_cold_load)
                .map_err(|error| cx.state.fail(error))?
                .ok_or(InstrStop::InvalidJump)?;
            if load.is_cold {
                cx.gas.spend(cold_cost)?;
            }
            if !load.chunk.is_jumpdest(local_target) {
                cold_path();
                return Err(InstrStop::InvalidJump);
            }
            cx.state.activate_code_chunk(chunk_index, load.chunk.into_bytecode());
        }
        local_target
    } else {
        target
    };
    if !cx.state.bytecode().is_valid_jumpdest(local_target) {
        cold_path();
        return Err(InstrStop::InvalidJump);
    }
    unsafe { cx.pc.set_unchecked(cx.state.bytecode(), local_target) };
    Ok(())
}

#[instruction]
pub fn pc(cx: _) -> out {
    *out = Word::from(cx.state.global_pc(*cx.pc));
}

#[instruction(dynamic_gas)]
pub fn gas(cx: _) -> out {
    *out = Word::from(cx.gas.remaining());
}

#[instruction]
pub fn jumpdest() {}

#[instruction(dynamic_gas)]
pub fn r#return(cx: _, [offset, len]: [Word]) -> Result {
    return_inner(cx, offset, len, InstrStop::Return)
}

#[instruction(dynamic_gas)]
pub fn revert(cx: _, [offset, len]: [Word]) -> Result {
    return_inner(cx, offset, len, InstrStop::Revert)
}

#[inline]
fn return_inner<T: EvmTypesHost>(
    cx: GasInstructionCx<'_, '_, '_, T>,
    offset: &Word,
    len: &Word,
    result: InstrStop,
) -> Result {
    let len = word_to_usize(*len)?;
    let output = if len != 0 {
        let offset = word_to_usize(*offset)?;
        let Some(end) = offset.checked_add(len) else {
            return Err(InstrStop::MemoryOOG);
        };
        if end > u32::MAX as usize {
            return Err(InstrStop::MemoryLimitOOG);
        }
        cx.state.resize_memory(cx.gas, offset, len)?;
        offset as u32..end as u32
    } else {
        0..0
    };
    cx.state.set_output(output);
    Err(result)
}

#[instruction]
pub fn invalid(cx: _) -> Result {
    cold_path();
    let opcode = cx.pc.op();
    Err(if opcode == op::INVALID {
        InstrStop::InvalidFEOpcode
    } else if OpCode::new(opcode).is_some() {
        InstrStop::NotActivated
    } else {
        InstrStop::OpcodeNotFound
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfigSelector, EvmFeatures, ExecutionConfig, SpecId,
        bytecode::chunks::{CODE_CHUNK_SIZE, chunkify_code},
        env::TxEnvExt,
        interpreter::{Interpreter, MessageExt},
        test_utils::{RunConfig, TestHost, TestTypes, push, run, run_stack},
    };
    use alloc::vec::Vec;
    use alloy_primitives::B256;
    use core::assert_matches;

    #[test]
    fn stop_opcode() {
        let interp = run(RunConfig::new([op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);

        let interp = run(RunConfig::new([op::STOP, op::INVALID]));
        assert_matches!(interp.err, InstrStop::Stop);
    }

    #[test]
    fn invalid_opcode() {
        for gas_limit in [0, 100] {
            for (opcode, expected) in [
                (op::INVALID, InstrStop::InvalidFEOpcode),
                (0x0c, InstrStop::OpcodeNotFound),
                (op::PUSH0, InstrStop::NotActivated),
                (op::TSTORE, InstrStop::NotActivated),
                (op::DUPN, InstrStop::NotActivated),
            ] {
                let interp =
                    run(RunConfig::new([opcode]).spec(SpecId::LONDON).gas_limit(gas_limit));
                assert_eq!(interp.err, expected, "opcode={opcode:#x}, gas={gas_limit}");
                assert_eq!(interp.gas_remaining(), gas_limit);
                assert_eq!(interp.stack_len, 0);

                let interp = run(RunConfig::new([op::PUSH1, 0, op::POP, opcode])
                    .spec(SpecId::LONDON)
                    .gas_limit(gas_limit + 5));
                assert_eq!(interp.err, expected, "opcode={opcode:#x}, gas={gas_limit}");
                assert_eq!(interp.gas_remaining(), gas_limit);
                assert_eq!(interp.stack_len, 0);
            }
        }
    }

    #[test]
    fn jump_opcode() {
        let interp = run(RunConfig::new([op::PUSH1, 0x03, op::JUMP, op::JUMPDEST, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);

        let interp = run(RunConfig::new([op::PUSH1, 0x00, op::JUMP, op::JUMPDEST, op::STOP]));
        assert_matches!(interp.err, InstrStop::InvalidJump);

        let mut code = Vec::new();
        push(&mut code, Word::MAX);
        code.push(op::JUMP);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::InvalidJump);

        let interp =
            run(RunConfig::new([op::PUSH1, 0x04, op::JUMP, op::STOP, op::JUMPDEST, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
    }

    #[test]
    fn jumpi_opcode() {
        let interp = run(RunConfig::new([
            op::PUSH1,
            0x01,
            op::PUSH1,
            0x06,
            op::JUMPI,
            op::STOP,
            op::JUMPDEST,
            op::STOP,
        ]));
        assert_matches!(interp.err, InstrStop::Stop);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0x00,
            op::PUSH1,
            0x06,
            op::JUMPI,
            op::JUMPDEST,
            op::STOP,
        ]));
        assert_matches!(interp.err, InstrStop::Stop);

        let interp =
            run(RunConfig::new([op::PUSH1, 0x01, op::PUSH1, 0x05, op::JUMPI, op::STOP, op::STOP]));
        assert_matches!(interp.err, InstrStop::InvalidJump);

        let mut code = Vec::new();
        push(&mut code, 1);
        push(&mut code, Word::MAX);
        code.push(op::JUMPI);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::InvalidJump);
    }

    #[test]
    fn cross_chunk_jump_loads_and_charges_cold_chunk() {
        let mut code = vec![op::STOP; CODE_CHUNK_SIZE + 3];
        code[..4].copy_from_slice(&[op::PUSH2, 0x30, 0x00, op::JUMP]);
        code[CODE_CHUNK_SIZE] = op::JUMPDEST;
        code[CODE_CHUNK_SIZE + 1] = op::PC;
        let chunks = chunkify_code(&code).unwrap();

        let run_chunked = |cold: bool| {
            let mut host = TestHost {
                code: code.clone().into(),
                code_chunks: chunks.clone(),
                ..TestHost::default()
            };
            if cold {
                host.cold_code_chunks.insert(1);
            }
            host.execution_config =
                ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
            host.execution_config.version.features.insert(EvmFeatures::BYTECODE_CHUNKING);
            let message = MessageExt {
                gas_limit: 10_000,
                code: chunks[0].bytecode().clone(),
                code_hash: B256::with_last_byte(1),
                code_size: code.len() as u32,
                code_chunk_index: 0,
                chunked_code: true,
                ..MessageExt::default()
            };
            let tx = TxEnvExt::default();
            let mut interpreter = Interpreter::<TestTypes>::new(&tx, &message);
            let outcome = interpreter.run(&mut host).unwrap();
            (outcome, interpreter.stack().to_vec(), interpreter.gas().remaining())
        };

        let warm = run_chunked(false);
        let cold = run_chunked(true);
        assert_eq!(warm.0, InstrStop::Stop);
        assert_eq!(warm.1, [Word::from(CODE_CHUNK_SIZE + 1)]);
        assert_eq!(cold.0, InstrStop::Stop);
        assert_eq!(cold.1, warm.1);
        assert_eq!(warm.2 - cold.2, code_chunk_gas(1).unwrap());
    }

    #[test]
    fn pc_opcode() {
        let interp = run(RunConfig::new([op::PC, op::JUMPDEST, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [0]);

        let interp = run(RunConfig::new([op::JUMPDEST, op::PC, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(1)]);
    }

    #[test]
    fn gas_opcode() {
        let interp = run(RunConfig::new([op::GAS, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack().len(), 1);
        assert!(interp.stack()[0] < Word::from(10_000));

        let interp = run(RunConfig::new([op::GAS, op::GAS, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack().len(), 2);
        assert!(interp.stack()[1] < interp.stack()[0]);
    }

    #[test]
    fn jumpdest_opcode() {
        let interp = run(RunConfig::new([op::JUMPDEST, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert!(interp.stack().is_empty());

        let interp = run(RunConfig::new([op::JUMPDEST, op::JUMPDEST, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
    }

    #[test]
    fn return_opcode() {
        let mut interp = run_stack([0, 0], op::RETURN);
        assert_matches!(interp.err, InstrStop::Return);
        assert!(interp.memory(0, 0).is_empty());
        assert!(interp.output().is_empty());

        let mut interp = run_stack([0, 1], op::RETURN);
        assert_matches!(interp.err, InstrStop::Return);
        assert_eq!(interp.memory(0, 1), [0]);
        assert_eq!(interp.output(), [0]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0xab));
        push(&mut code, Word::from(2));
        code.push(op::MSTORE8);
        push(&mut code, Word::from(3));
        push(&mut code, Word::from(2));
        code.push(op::RETURN);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::Return);
        assert_eq!(interp.output(), [0xab, 0, 0]);

        let interp = run_stack([Word::from(0), Word::MAX], op::RETURN);
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);

        let interp = run_stack([Word::from(u32::MAX), Word::from(1)], op::RETURN);
        assert_matches!(interp.err, InstrStop::MemoryLimitOOG);
    }

    #[test]
    fn revert_opcode() {
        let mut interp = run_stack([0, 0], op::REVERT);
        assert_matches!(interp.err, InstrStop::Revert);
        assert!(interp.memory(0, 0).is_empty());
        assert!(interp.output().is_empty());

        let mut interp = run_stack([2, 3], op::REVERT);
        assert_matches!(interp.err, InstrStop::Revert);
        assert_eq!(interp.memory(2, 3), [0, 0, 0]);
        assert_eq!(interp.output(), [0, 0, 0]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0xcd));
        push(&mut code, Word::from(4));
        code.push(op::MSTORE8);
        push(&mut code, Word::from(2));
        push(&mut code, Word::from(4));
        code.push(op::REVERT);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::Revert);
        assert_eq!(interp.output(), [0xcd, 0]);

        let interp = run_stack([Word::from(0), Word::MAX], op::REVERT);
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);

        let interp = run_stack([Word::from(u32::MAX), Word::from(1)], op::REVERT);
        assert_matches!(interp.err, InstrStop::MemoryLimitOOG);
    }
}
