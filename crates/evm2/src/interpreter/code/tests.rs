//! Whole initcode execution, account-source switching, and inspector positions.

use super::*;
use crate::{
    BaseEvmConfigSelector, ExecutionConfig, SpecId, Version,
    bytecode::{Bytecode, CODE_CHUNK_SIZE, MAX_INITCODE_SIZE},
    env::TxEnvExt,
    evm::inspector::Inspector,
    interpreter::{
        InstrStop, Interpreter, InterpreterPool, MessageExt, instructions::encode_rjump_offset, op,
    },
    test_utils::{TestHost, TestTypes},
};
use alloc::vec;
use alloy_primitives::Bytes;

const GAS_LIMIT: u64 = 10_000_000;

#[test]
fn relative_jumps_activate_with_either_feature() {
    let [high, low] = encode_rjump_offset(0).unwrap();
    let message = MessageExt {
        gas_limit: GAS_LIMIT,
        code: Bytecode::new_legacy(Bytes::from(vec![op::RJUMP, high, low, op::JUMPDEST, op::STOP]))
            .into(),
        ..Default::default()
    };
    let tx = TxEnvExt::default();
    for (version, expected) in [
        (Version::base(SpecId::OSAKA).with_rjump(true), InstrStop::Stop),
        (Version::base(SpecId::OSAKA).with_tip1143(true), InstrStop::Stop),
        (*Version::base(SpecId::OSAKA), InstrStop::OpcodeNotFound),
    ] {
        let mut host = TestHost {
            execution_config: ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(
                SpecId::OSAKA,
            )
            .with_version(version),
            ..Default::default()
        };
        let mut frame = Interpreter::<TestTypes>::new(&tx, &message);
        assert_eq!(frame.run(&mut host).unwrap(), expected);
    }
}

fn run_initcode(code: Vec<u8>, expected: InstrStop) -> (Vec<Word>, Bytes, u64) {
    let bytes = Bytes::from(code);
    let tx = TxEnvExt::default();
    let message = MessageExt {
        gas_limit: GAS_LIMIT,
        code: Bytecode::new_legacy(bytes.clone()).into(),
        ..Default::default()
    };
    let mut host = TestHost {
        execution_config: ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA)
            .with_version(Version::base(SpecId::OSAKA).with_tip1143(true).with_rjump(true)),
        ..Default::default()
    };
    let mut frame = Interpreter::<TestTypes>::new(&tx, &message);
    assert_eq!(frame.original_bytecode_slice(), bytes.as_ref());
    assert_eq!(frame.original_bytecode_hash(), alloy_primitives::keccak256(&bytes));
    assert_eq!(frame.run(&mut host).unwrap(), expected);
    assert!(matches!(frame.code.active(), CodeChunk::Direct(_)));
    assert_eq!(frame.original_bytecode_slice(), bytes.as_ref());
    (
        frame.stack().as_slice().to_vec(),
        Bytes::copy_from_slice(frame.output()),
        frame.gas().remaining(),
    )
}

#[test]
fn initcode_fallthrough_preserves_pc_and_gas() {
    let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE + 1];
    code.extend([op::PC, op::GAS, op::CODESIZE, op::STOP]);
    let size = code.len();
    let (stack, _, gas) = run_initcode(code, InstrStop::Stop);
    assert_eq!(
        stack,
        vec![
            Word::from(CODE_CHUNK_SIZE + 1),
            Word::from(GAS_LIMIT - (CODE_CHUNK_SIZE + 1) as u64 - 4),
            Word::from(size),
        ]
    );
    assert_eq!(gas, GAS_LIMIT - (CODE_CHUNK_SIZE + 1) as u64 - 6);
}

#[test]
fn initcode_crossing_push_and_truncated_immediates() {
    for continuation in 1..=32 {
        let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE - (33 - continuation)];
        code.push(op::PUSH32);
        code.extend([op::JUMPDEST; 32]);
        code.extend([op::PC, op::GAS, op::STOP]);
        let pc = CODE_CHUNK_SIZE + continuation;
        let (stack, _, _) = run_initcode(code, InstrStop::Stop);
        assert_eq!(stack[0], Word::from_be_bytes([op::JUMPDEST; 32]));
        assert_eq!(stack[1], Word::from(pc));
    }
    let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE - 1];
    code.extend([op::PUSH32, 0x12]);
    let (stack, _, _) = run_initcode(code, InstrStop::Stop);
    assert_eq!(stack, [Word::from(0x12) << 248]);
}

#[test]
fn initcode_jumps_forward_and_back() {
    let target = 3 * CODE_CHUNK_SIZE;
    let mut code = vec![op::STOP; target + 7];
    code[..10].copy_from_slice(&[
        op::PUSH3,
        (target >> 16) as u8,
        (target >> 8) as u8,
        target as u8,
        op::JUMP,
        op::JUMPDEST,
        op::PC,
        op::GAS,
        op::STOP,
        op::STOP,
    ]);
    code[target..].copy_from_slice(&[
        op::JUMPDEST,
        op::PC,
        op::PUSH1,
        5,
        op::JUMP,
        op::INVALID,
        op::STOP,
    ]);
    let (stack, _, _) = run_initcode(code, InstrStop::Stop);
    assert_eq!(&stack[..2], [Word::from(target + 1), Word::from(6)]);
}

#[test]
fn initcode_rejects_jump_into_continued_push() {
    let target = CODE_CHUNK_SIZE;
    let mut code = vec![op::STOP; target + 33];
    code[..4].copy_from_slice(&[op::PUSH2, (target >> 8) as u8, target as u8, op::JUMP]);
    code[target - 1] = op::PUSH32;
    code[target] = op::JUMPDEST;
    run_initcode(code, InstrStop::InvalidJump);
}

#[test]
fn initcode_untaken_jump_does_not_validate_destination() {
    let mut code = vec![op::INVALID; MAX_INITCODE_SIZE];
    code[..5].copy_from_slice(&[op::PUSH0, op::PUSH1, 99, op::JUMPI, op::STOP]);
    run_initcode(code, InstrStop::Stop);
}

#[test]
fn initcode_relative_jump_crosses_boundary() {
    let mut code = vec![op::JUMPDEST; CODE_CHUNK_SIZE - 1];
    code.push(op::RJUMP);
    code.extend(encode_rjump_offset(1).unwrap());
    code.extend([op::INVALID, op::JUMPDEST, op::PC, op::GAS, op::STOP]);
    let (stack, _, _) = run_initcode(code, InstrStop::Stop);
    assert_eq!(stack[0], Word::from(CODE_CHUNK_SIZE + 4));
}

#[test]
fn initcode_codecopy_reads_original_bytes_and_zero_fills() {
    for offset in [CODE_CHUNK_SIZE - 16, MAX_INITCODE_SIZE - 16, MAX_INITCODE_SIZE + 1] {
        let mut code = vec![0xab; MAX_INITCODE_SIZE];
        code[..13].copy_from_slice(&[
            op::PUSH1,
            64,
            op::PUSH3,
            (offset >> 16) as u8,
            (offset >> 8) as u8,
            offset as u8,
            op::PUSH0,
            op::CODECOPY,
            op::PUSH1,
            64,
            op::PUSH0,
            op::RETURN,
            op::STOP,
        ]);
        let mut expected = vec![0; 64];
        if offset < code.len() {
            let count = (code.len() - offset).min(64);
            expected[..count].copy_from_slice(&code[offset..offset + count]);
        }
        let (_, output, _) = run_initcode(code, InstrStop::Return);
        assert_eq!(output.as_ref(), expected);
    }
}

#[test]
fn initcode_preserves_legacy_jump_analysis_inside_invalid_relative_immediates() {
    for (target, operand, expected) in [
        (CODE_CHUNK_SIZE + 1, op::STOP, InstrStop::Stop),
        (CODE_CHUNK_SIZE + 20, op::PUSH32, InstrStop::InvalidJump),
    ] {
        let mut code = vec![op::STOP; CODE_CHUNK_SIZE + 40];
        code[..4].copy_from_slice(&[op::PUSH2, (target >> 8) as u8, target as u8, op::JUMP]);
        code[CODE_CHUNK_SIZE - 1] = op::RJUMP;
        code[CODE_CHUNK_SIZE] = operand;
        code[target] = op::JUMPDEST;
        run_initcode(code, expected);
    }
}

#[derive(Default)]
struct InitcodeTrace(Vec<(bool, usize, u64)>);

impl Inspector<TestTypes> for InitcodeTrace {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, TestTypes>) {
        self.0.push((false, interp.pc(), interp.gas().remaining()));
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, TestTypes>) {
        self.0.push((true, interp.pc(), interp.gas().remaining()));
    }
}

#[test]
fn initcode_inspector_reports_full_input_and_global_pc() {
    let target = MAX_INITCODE_SIZE - 4;
    let mut code = vec![op::STOP; MAX_INITCODE_SIZE];
    code[..5].copy_from_slice(&[
        op::PUSH3,
        (target >> 16) as u8,
        (target >> 8) as u8,
        target as u8,
        op::JUMP,
    ]);
    code[target..].copy_from_slice(&[op::JUMPDEST, op::PC, op::CODESIZE, op::STOP]);
    let tx = TxEnvExt::default();
    let message = MessageExt {
        gas_limit: 100_000,
        code: Bytecode::new_legacy(code.into()).into(),
        ..Default::default()
    };
    let mut host = TestHost::default();
    let mut trace = InitcodeTrace::default();
    let mut frame = Interpreter::<TestTypes>::new(&tx, &message);
    assert_eq!(frame.run_inspect(&mut host, &mut trace).unwrap(), InstrStop::Stop);
    let pcs = trace.0.iter().filter_map(|&(end, pc, _)| (!end).then_some(pc)).collect::<Vec<_>>();
    assert_eq!(pcs, [0, 4, target, target + 1, target + 2, target + 3]);
    assert_eq!(frame.stack().as_slice(), [Word::from(target + 1), Word::from(MAX_INITCODE_SIZE)]);
}

#[test]
fn inspector_reports_logical_chunk_instructions() {
    let boundary = CODE_CHUNK_SIZE;
    let target = boundary - 1;
    let mut code = vec![op::STOP; boundary + 40];
    code[..4].copy_from_slice(&[op::PUSH2, (target >> 8) as u8, target as u8, op::JUMP]);
    code[target] = op::JUMPDEST;
    code[boundary] = op::JUMPDEST;
    let code = Bytes::from(code);
    let original_hash = alloy_primitives::keccak256(&code);
    let message = MessageExt {
        gas_limit: GAS_LIMIT,
        code: crate::bytecode::code_chunk(&code, 0).unwrap(),
        ..Default::default()
    };
    let mut host = TestHost {
        code,
        execution_config: ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA)
            .with_version(Version::base(SpecId::OSAKA).with_tip1143(true).with_rjump(true)),
        ..Default::default()
    };
    let mut trace = InitcodeTrace::default();
    let tx = TxEnvExt::default();
    let mut frame = Interpreter::<TestTypes>::new(&tx, &message);
    assert_eq!(frame.original_bytecode_hash(), alloy_primitives::keccak256(message.code.bytes()));
    assert_ne!(frame.original_bytecode_hash(), original_hash);
    assert_eq!(frame.run_inspect(&mut host, &mut trace).unwrap(), InstrStop::Stop);
    assert_eq!(
        frame.original_bytecode_hash(),
        alloy_primitives::keccak256(frame.original_bytecode())
    );
    let steps = trace.0.iter().filter_map(|&(end, pc, _)| (!end).then_some(pc)).collect::<Vec<_>>();
    assert_eq!(steps, [0, 3, target, boundary, boundary + 1]);
    assert_eq!(trace.0.len(), steps.len() * 2);
    assert!(trace.0.as_chunks::<2>().0.iter().all(|pair| !pair[0].0 && pair[1].0));
}

#[test]
fn pooled_frames_switch_between_account_chunks_and_whole_bytecode() {
    let target = CODE_CHUNK_SIZE;
    let mut bytes = vec![op::STOP; target + 4];
    bytes[..4].copy_from_slice(&[op::PUSH2, (target >> 8) as u8, target as u8, op::JUMP]);
    bytes[target..].copy_from_slice(&[op::JUMPDEST, op::PC, op::CODESIZE, op::STOP]);
    let bytes = Bytes::from(bytes);
    let small = Bytes::from_static(&[op::PC, op::CODESIZE, op::STOP]);
    let large_result = [Word::from(target + 1), Word::from(bytes.len())];
    let small_result = [Word::ZERO, Word::from(small.len())];
    let cases = [
        (Bytecode::new_legacy(bytes.clone()).into(), large_result.as_slice()),
        (Bytecode::new_legacy(small.clone()).into(), small_result.as_slice()),
        (crate::bytecode::code_chunk(&bytes, 0).unwrap(), large_result.as_slice()),
        (Bytecode::new_legacy(small).into(), small_result.as_slice()),
        (CodeChunk::default(), &[]),
    ];
    let mut host = TestHost {
        code: bytes,
        execution_config: ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA)
            .with_version(Version::base(SpecId::OSAKA).with_tip1143(true).with_rjump(true)),
        ..Default::default()
    };
    let tx = TxEnvExt::default();
    let mut pool = InterpreterPool::<TestTypes>::new();
    for _ in 0..2 {
        for (code, expected) in &cases {
            let message =
                MessageExt { code: code.clone(), gas_limit: 100_000, ..Default::default() };
            let mut frame = pool.pop(&tx, &message);
            assert_eq!(frame.pc(), 0);
            assert_eq!(frame.run(&mut host).unwrap(), InstrStop::Stop);
            assert_eq!(frame.stack().as_slice(), *expected);
            pool.push(frame);
        }
    }
}
