//! Execution proofs for the agreed overlap layout and valid replacement destinations.

use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::{
        Bytecode, CODE_CHUNK_SIZE, COLD_CODE_CHUNK_GAS, CodeChunk, code_chunk, code_metadata,
    },
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, Database, Db},
    interpreter::{GasTracker, InstrStop, Interpreter, MessageExt, Word},
    registry::TxRegistry,
};
use std::{cell::RefCell, convert::Infallible, rc::Rc};

#[derive(Clone)]
struct Provider {
    raw: Bytes,
    reads: Rc<RefCell<Vec<u32>>>,
}

impl Database for Provider {
    type Error = Infallible;

    fn get_account(&mut self, _: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(Some(AccountInfo {
            code_hash: keccak256(&self.raw),
            code_metadata: code_metadata(&self.raw).unwrap(),
            code: None,
            ..Default::default()
        }))
    }
    fn get_code_kind_by_hash(
        &mut self,
        _: &B256,
    ) -> Result<evm2::bytecode::BytecodeKind, Self::Error> {
        Ok(evm2::bytecode::BytecodeKind::Legacy)
    }
    fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::new_legacy(self.raw.clone()))
    }
    fn get_code_chunk_by_hash(
        &mut self,
        _: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        self.reads.borrow_mut().push(index);
        Ok(code_chunk(&self.raw, index))
    }
    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }
    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

fn execute(raw: Vec<u8>, enabled: bool, budget: u64) -> (InstrStop, Bytes, u64, Vec<u32>) {
    execute_spec(raw, enabled, budget, SpecId::PRAGUE)
}

fn execute_spec(
    raw: Vec<u8>,
    enabled: bool,
    budget: u64,
    spec: SpecId,
) -> (InstrStop, Bytes, u64, Vec<u32>) {
    let reads = Rc::new(RefCell::new(Vec::new()));
    let provider = Provider { raw: raw.into(), reads: reads.clone() };
    let version = Version::new(spec).with_tip1143(enabled);
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(spec, version),
        spec,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(provider),
        Precompiles::base(spec),
    );
    let mut gas = GasTracker::new(budget);
    let frame = prepare_initial_frame(
        &mut evm,
        Address::repeat_byte(0x55),
        0,
        TxKind::Call(Address::repeat_byte(0x44)),
        &Bytes::new(),
        U256::ZERO,
        &mut gas,
    )
    .unwrap();
    let result =
        execute_initial_frame(&mut evm, &TxEnvExt::default(), frame, &mut gas, budget, 0).unwrap();
    let seen = reads.borrow().clone();
    (result.stop, result.output, gas.spent(), seen)
}

fn push3(code: &mut Vec<u8>, target: usize) {
    code.extend([0x62, (target >> 16) as u8, (target >> 8) as u8, target as u8]);
}

#[test]
fn all_push_widths_cross_without_changing_values_or_instruction_gas() {
    for width in 1..=32 {
        for distance in [1, width] {
            let mut code = vec![0x5b; CODE_CHUNK_SIZE - distance];
            code.push(0x5f + width as u8);
            code.extend((0..width).map(|i| (i * 17 + 3) as u8));
            code.extend([0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
            let reference = execute(code.clone(), false, 200_000);
            let actual = execute(code, true, 200_000);
            assert_eq!(actual.0, InstrStop::Return, "width={width}, distance={distance}");
            assert_eq!(actual.1, reference.1);
            assert_eq!(actual.2, reference.2 + 2 * COLD_CODE_CHUNK_GAS);
            assert_eq!(actual.3, [0, 1]);
        }
    }
}

#[test]
fn fallthrough_with_full_stack_into_non_jumpdest() {
    let mut code = vec![0x5f; 1024];
    code.resize(CODE_CHUNK_SIZE, 0x5b);
    code.extend([0x50, 0]);
    let reference = execute(code.clone(), false, 200_000);
    let actual = execute(code, true, 200_000);
    assert_eq!(actual.0, InstrStop::Stop);
    assert_eq!(actual.2, reference.2 + 2 * COLD_CODE_CHUNK_GAS);
    assert_eq!(actual.3, [0, 1]);
}

#[test]
fn both_jumps_accept_every_replacement_and_untaken_jumpi_does_not_read() {
    for (op, cond) in [(0x56, 1), (0x57, 1), (0x57, 0)] {
        for offset in 0..32 {
            let mut code = if op == 0x57 { vec![0x60, cond] } else { Vec::new() };
            push3(&mut code, CODE_CHUNK_SIZE + offset);
            code.extend([op, 0]);
            code.resize(CODE_CHUNK_SIZE + 33, 0);
            code[CODE_CHUNK_SIZE - 1] = 0x7f;
            let actual = execute(code.clone(), true, 200_000);
            assert_eq!(actual.0, InstrStop::Stop);
            assert_eq!(actual.3, if cond == 0 { vec![0] } else { vec![0, 1] });
            if cond != 0 {
                assert_eq!(execute(code, false, 200_000).0, InstrStop::InvalidJump);
                let opcode_gas = if op == 0x56 { 3 + 8 } else { 3 + 3 + 10 };
                assert_eq!(actual.2, 2 * COLD_CODE_CHUNK_GAS + opcode_gas + (32 - offset) as u64);
            }
        }
    }
}

#[test]
fn crossing_final_push_stops_without_loading_a_data_only_final_chunk() {
    let mut code = vec![0x5b; CODE_CHUNK_SIZE - 1];
    code.extend([0x7f, 0xab]);
    let reference = execute(code.clone(), false, 200_000);
    let actual = execute(code, true, 200_000);
    assert_eq!(actual.0, InstrStop::Stop);
    assert_eq!(actual.2, reference.2 + COLD_CODE_CHUNK_GAS);
    assert_eq!(actual.3, [0]);
}

#[test]
fn unaffordable_remote_jump_never_fetches_destination() {
    for op in [0x56, 0x57] {
        let mut code = if op == 0x57 { vec![0x60, 1] } else { Vec::new() };
        push3(&mut code, CODE_CHUNK_SIZE);
        code.push(op);
        code.resize(CODE_CHUNK_SIZE + 2, 0);
        code[CODE_CHUNK_SIZE] = 0x5b;
        let actual = execute(code, true, 2 * COLD_CODE_CHUNK_GAS - 1);
        assert_eq!(actual.0, InstrStop::OutOfGas);
        assert_eq!(actual.3, [0]);
    }
}

#[test]
fn codecopy_returns_original_prefix_not_replacement_jumpdest() {
    let mut code = vec![0x60, 32];
    push3(&mut code, CODE_CHUNK_SIZE);
    code.extend([0x5f, 0x39, 0x60, 32, 0x5f, 0xf3]);
    code.resize(CODE_CHUNK_SIZE + 33, 0);
    code[CODE_CHUNK_SIZE - 1] = 0x7f;
    for (i, byte) in code[CODE_CHUNK_SIZE..CODE_CHUNK_SIZE + 32].iter_mut().enumerate() {
        *byte = i as u8;
    }
    let actual = execute(code, true, 200_000);
    assert_eq!(actual.0, InstrStop::Return);
    assert_eq!(actual.1.as_ref(), (0..32).collect::<Vec<u8>>());
    assert_eq!(actual.3, [0, 1]);
}

#[test]
fn inspector_omits_generated_transfer_and_keeps_global_pc() {
    struct Trace(Rc<RefCell<Vec<(usize, u8)>>>);
    impl evm2::evm::inspector::Inspector<BaseEvmTypes> for Trace {
        fn step(&mut self, interpreter: &mut evm2::interpreter::Interpreter<'_, '_, BaseEvmTypes>) {
            self.0.borrow_mut().push((interpreter.pc(), interpreter.opcode()));
        }
    }
    let mut code = vec![0x5b; CODE_CHUNK_SIZE - 1];
    code.push(0x7f);
    code.extend(0..32);
    code.extend([0x50, 0x58, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
    let expected_bytes = code.clone();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let steps = Rc::new(RefCell::new(Vec::new()));
    let version = Version::new(SpecId::PRAGUE).with_tip1143(true);
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(Provider { raw: code.into(), reads: reads.clone() }),
        Precompiles::base(SpecId::PRAGUE),
    );
    evm.set_inspector(Trace(steps.clone()));
    let mut gas = GasTracker::new(200_000);
    let frame = prepare_initial_frame(
        &mut evm,
        Address::repeat_byte(0x55),
        0,
        TxKind::Call(Address::repeat_byte(0x44)),
        &Bytes::new(),
        U256::ZERO,
        &mut gas,
    )
    .unwrap();
    let result =
        execute_initial_frame(&mut evm, &TxEnvExt::default(), frame, &mut gas, 200_000, 0).unwrap();
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(U256::from_be_slice(&result.output), U256::from(CODE_CHUNK_SIZE + 33));
    assert_eq!(*reads.borrow(), [0, 1]);
    let mut expected = Vec::new();
    let mut pc = 0;
    while pc < expected_bytes.len() {
        let op = expected_bytes[pc];
        expected.push((pc, op));
        pc += 1 + if (0x60..=0x7f).contains(&op) { usize::from(op - 0x5f) } else { 0 };
    }
    assert_eq!(*steps.borrow(), expected);
}

#[test]
fn alternative_jump_entry_uses_its_own_push_overlap() {
    for spec in [SpecId::PRAGUE, SpecId::AMSTERDAM] {
        for boundary in [CODE_CHUNK_SIZE, CODE_CHUNK_SIZE * 2] {
            for op in [0x56, 0x57] {
                let mut code = if op == 0x57 { vec![0x60, 1] } else { Vec::new() };
                push3(&mut code, boundary - 2);
                code.push(op);
                code.resize(boundary - 5, 0);
                // Linear execution sees DUPN's immediate and then PUSH32. The original
                // PUSH-only jump map also admits the later JUMPDEST/PUSH32 entry.
                code.extend([0xe6, 0x60, 0x7f, 0x5b, 0x7f]);
                let expected: Vec<u8> = (0..32).map(|i| i * 7 + 1).collect();
                code.extend(&expected);
                code.extend([0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
                let reference = execute_spec(code.clone(), false, 300_000, spec);
                let actual = execute_spec(code, true, 300_000, spec);
                assert_eq!(actual.0, InstrStop::Return, "{spec:?}, {boundary}, {op}");
                assert_eq!(actual.1.as_ref(), expected);
                assert_eq!(actual.1, reference.1);
                let chunks = if boundary == CODE_CHUNK_SIZE { 2 } else { 3 };
                assert_eq!(actual.2, reference.2 + chunks * COLD_CODE_CHUNK_GAS);
            }
        }
    }
}

#[test]
fn public_pc_setter_selects_padding_and_rejects_internal_bytes() {
    let mut raw = vec![0; CODE_CHUNK_SIZE - 5];
    raw.extend([0xe6, 0x60, 0x7f, 0x5b, 0x7f]);
    raw.extend([0xab; 32]);
    raw.extend([0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
    let raw = Bytes::from(raw);
    let chunk = code_chunk(&raw, 0).unwrap();
    let message = MessageExt {
        gas_limit: 200_000,
        code: chunk.bytecode(true),
        code_chunk: Some(chunk),
        code_size: raw.len(),
        code_hash: keccak256(&raw),
        code_address: Address::repeat_byte(0x44),
        ..MessageExt::default()
    };
    let tx = TxEnvExt::default();
    let mut interp = Interpreter::<'_, '_, BaseEvmTypes>::new(&tx, &message);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            interp.set_pc(CODE_CHUNK_SIZE);
        }))
        .is_err()
    );
    interp.set_pc(CODE_CHUNK_SIZE - 2);
    assert_eq!(interp.pc(), CODE_CHUNK_SIZE - 2);
    assert_eq!(interp.opcode(), 0x5b);
    let provider = Provider { raw, reads: Rc::new(RefCell::new(Vec::new())) };
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(
            SpecId::PRAGUE,
            Version::new(SpecId::PRAGUE).with_tip1143(true),
        ),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(provider),
        Precompiles::base(SpecId::PRAGUE),
    );
    assert_eq!(interp.run(&mut evm).unwrap(), InstrStop::Return);
    assert_eq!(interp.output(), &[0xab; 32]);
}
