//! Execution proofs for immutable overlap buffers and trailing start offsets.

use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::{
        Bytecode, CODE_CHUNK_SIZE, COLD_CODE_CHUNK_GAS, CodeChunk, code_chunk, code_metadata,
    },
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, Database, Db, SystemTx},
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
            extension: (code_metadata(&self.raw).unwrap())
                .map(evm2::evm::AccountExtension::chunked)
                .unwrap_or_default(),
            code: None,
            ..Default::default()
        }))
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
fn all_crossing_pushes_preserve_values_and_charge_padding_and_transfer() {
    for width in 1..=32 {
        for distance in 1..=width {
            let mut code = vec![0x5b; CODE_CHUNK_SIZE - distance];
            code.push(0x5f + width as u8);
            code.extend((0..width).map(|i| (i * 17 + 3) as u8));
            code.extend([0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
            let reference = execute(code.clone(), false, 200_000);
            let actual = execute(code, true, 200_000);
            assert_eq!(actual.0, InstrStop::Return, "width={width}, distance={distance}");
            assert_eq!(actual.1, reference.1);
            let unused_padding = 32 - (width + 1 - distance);
            assert_eq!(
                actual.2,
                reference.2 + 2 * COLD_CODE_CHUNK_GAS + 2 + unused_padding.min(3) as u64
            );
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
    assert_eq!(actual.2, reference.2 + 2 * COLD_CODE_CHUNK_GAS + 5);
    assert_eq!(actual.3, [0, 1]);
}

#[test]
fn both_jumps_reject_leading_data_and_untaken_jumpi_does_not_read() {
    for (op, cond) in [(0x56, 1), (0x57, 1), (0x57, 0)] {
        for offset in 0..32 {
            let mut code = if op == 0x57 { vec![0x60, cond] } else { Vec::new() };
            push3(&mut code, CODE_CHUNK_SIZE + offset);
            code.extend([op, 0]);
            code.resize(CODE_CHUNK_SIZE + 33, 0);
            code[CODE_CHUNK_SIZE - 1] = 0x7f;
            let actual = execute(code.clone(), true, 200_000);
            assert_eq!(actual.0, if cond == 0 { InstrStop::Stop } else { InstrStop::InvalidJump });
            assert_eq!(actual.3, if cond == 0 { vec![0] } else { vec![0, 1] });
            if cond != 0 {
                assert_eq!(execute(code, false, 200_000).0, InstrStop::InvalidJump);
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
fn codecopy_reads_original_prefix_without_previous_chunk() {
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
fn alternative_jump_entries_execute_the_stored_buffer_without_rewriting() {
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
                // This unusual entry consumes padding bytes as PUSH data before the tail.
                // The tail uses the canonical successor offset, where the remaining data is
                // invalid. The canonical buffer stays immutable instead of being
                // rebuilt for the entry.
                assert_eq!(reference.0, InstrStop::Return);
                assert_eq!(actual.0, InstrStop::OpcodeNotFound, "{spec:?}, {boundary}, {op}");
                assert!(actual.1.is_empty());
            }
        }
    }
}

#[test]
fn public_pc_setter_preserves_buffer_and_rejects_internal_bytes() {
    let mut raw = vec![0; CODE_CHUNK_SIZE - 5];
    raw.extend([0xe6, 0x60, 0x7f, 0x5b, 0x7f]);
    raw.extend([0xab; 32]);
    raw.extend([0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
    let raw = Bytes::from(raw);
    let chunk = code_chunk(&raw, 0).unwrap();
    let message = MessageExt {
        gas_limit: 200_000,
        is_lazy_code: true,
        code_chunk: chunk,
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
    let allocation = interp.original_bytecode().as_ptr();
    interp.set_pc(CODE_CHUNK_SIZE - 2);
    assert_eq!(interp.original_bytecode().as_ptr(), allocation);
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
    assert_eq!(interp.run(&mut evm).unwrap(), InstrStop::OpcodeNotFound);
    assert!(interp.output().is_empty());
}

#[test]
fn rjump_requires_jumpdest_and_remains_gated() {
    assert_eq!(execute(vec![0xe0, 0x80, 0x80, 0], true, 100_000).0, InstrStop::InvalidJump);
    let code = vec![0xe0, 0x80, 0x80, 0x5b, 0];
    let actual = execute(code.clone(), true, 100_000);
    assert_eq!(actual.0, InstrStop::Stop);
    assert_eq!(actual.2, COLD_CODE_CHUNK_GAS + 3);
    assert_eq!(execute(code, false, 100_000).0, InstrStop::OpcodeNotFound);
}

#[test]
fn rjump_negative_offset_loops_until_gas_runs_out() {
    // -4 returns to JUMPDEST. Ordinary opcode costs bound execution.
    let actual = execute(vec![0x5b, 0xe0, 0x5a, 0x57], true, COLD_CODE_CHUNK_GAS + 6);
    assert_eq!(actual.0, InstrStop::OutOfGas);
    assert_eq!(actual.2, COLD_CODE_CHUNK_GAS + 6);
    assert!(actual.3.is_empty());
}

#[test]
fn rjump_rejects_bad_immediates_and_out_of_bounds_targets() {
    for code in [vec![0xe0, 0x5b, 0], vec![0xe0, 0x80, 0x60]] {
        assert_eq!(execute(code, true, 100_000).0, InstrStop::InvalidImmediateEncoding);
    }
    for code in [
        vec![0xe0, 0x80, 0x80], // +0 targets the end of code.
        vec![0xe0, 0x5a, 0x57], // -4 targets before code.
        vec![0xe0],             // Truncated immediate is safely padded, then range checked.
    ] {
        let actual = execute(code, true, 100_000);
        assert_eq!(actual.0, InstrStop::InvalidJump);
        assert!(actual.3.is_empty());
    }
}

#[test]
fn rjump_forward_and_backward_cross_chunks_require_jumpdest() {
    let mut code = Vec::new();
    push3(&mut code, CODE_CHUNK_SIZE - 4);
    code.push(0x56);
    code.resize(CODE_CHUNK_SIZE - 7, 0);
    code.extend([0x5b, 0, 0]); // Backward destination then STOP.
    code.extend([0x5b, 0xe0, 0x80, 0x81]); // +1 reaches next chunk offset one.
    code.extend([0xfe, 0x5b, 0xe0, 0x5a, 0x4f]); // -12 returns to boundary -7.
    let actual = execute(code, true, 100_000);
    assert_eq!(actual.0, InstrStop::Stop);
    assert_eq!(actual.2, 2 * COLD_CODE_CHUNK_GAS + 1000 + 3 + 8 + 3 + 4);
    assert_eq!(actual.3, [0, 1]);
}

#[test]
fn rjump_reserves_remote_chunk_gas_before_fetching() {
    let mut code = Vec::new();
    push3(&mut code, CODE_CHUNK_SIZE - 4);
    code.push(0x56);
    code.resize(CODE_CHUNK_SIZE - 4, 0);
    code.extend([0x5b, 0xe0, 0x80, 0x80, 0x5b, 0]);
    let actual = execute(code, true, 2 * COLD_CODE_CHUNK_GAS + 13);
    assert_eq!(actual.0, InstrStop::OutOfGas);
    assert_eq!(actual.3, [0]);
}

#[test]
fn rjump_immediates_can_cross_the_slice_boundary() {
    for distance in [1, 2] {
        let mut code = Vec::new();
        push3(&mut code, CODE_CHUNK_SIZE - distance - 1);
        code.push(0x56);
        code.resize(CODE_CHUNK_SIZE - distance - 1, 0);
        code.extend([0x5b, 0xe0, 0x80, 0x80, 0x5b, 0]);
        let actual = execute(code, true, 100_000);
        assert_eq!(actual.0, InstrStop::Stop);
        assert_eq!(actual.2, 2 * COLD_CODE_CHUNK_GAS + 3 + 8 + 2 + 2);
        assert_eq!(actual.3, [0, 1]);
    }
}

#[test]
fn standalone_prepared_bytecode_rejects_relative_entries_into_push_data() {
    let mut raw = Vec::new();
    push3(&mut raw, 1000);
    raw.push(0x56);
    raw.resize(CODE_CHUNK_SIZE + 2, 0);
    // At 1001, jump into PUSH1's immediate at the last payload byte. The ordinary
    // path has no spill; this alternative PUSH32 entry needs full zero padding.
    let offset = CODE_CHUNK_SIZE - 1 - 1004;
    raw[1000..1004].copy_from_slice(&[
        0x5b,
        0xe0,
        ((offset / 219 + 128) % 256) as u8,
        ((offset % 219 + 128) % 256) as u8,
    ]);
    raw[CODE_CHUNK_SIZE - 2..CODE_CHUNK_SIZE].copy_from_slice(&[0x60, 0x7f]);
    let raw = Bytes::from(raw);
    let chunk = code_chunk(&raw, 0).unwrap();
    let message = MessageExt {
        gas_limit: 100_000,
        code_chunk: (chunk.bytecode()).into(),
        ..MessageExt::default()
    };
    let tx = TxEnvExt::default();
    let mut interp = Interpreter::<'_, '_, BaseEvmTypes>::new(&tx, &message);
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(
            SpecId::PRAGUE,
            Version::new(SpecId::PRAGUE).with_tip1143(true),
        ),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(Provider { raw, reads: Rc::new(RefCell::new(Vec::new())) }),
        Precompiles::base(SpecId::PRAGUE),
    );
    assert_eq!(interp.run(&mut evm).unwrap(), InstrStop::InvalidJump);
    assert!(interp.stack().is_empty());
}

#[test]
fn rjumpi_branches_on_nonzero_and_consumes_one_condition() {
    for condition in [0, 1, 255] {
        let actual = execute(vec![0x60, condition, 0xe1, 0x80, 0x81, 0xfe, 0x5b, 0], true, 100_000);
        assert_eq!(
            actual.0,
            if condition == 0 { InstrStop::InvalidFEOpcode } else { InstrStop::Stop }
        );
        if condition != 0 {
            assert_eq!(actual.2, COLD_CODE_CHUNK_GAS + 8);
        }
        // +0 reaches JUMPDEST then POP: the condition must already be consumed.
        assert_eq!(
            execute(vec![0x60, condition, 0xe1, 0x80, 0x80, 0x5b, 0x50], true, 100_000).0,
            InstrStop::StackUnderflow
        );
    }
    assert_eq!(execute(vec![0xe1, 0x80, 0x80], true, 100_000).0, InstrStop::StackUnderflow);
    assert_eq!(execute(vec![0xe1], false, 100_000).0, InstrStop::OpcodeNotFound);
}

#[test]
fn rjumpi_checks_encoding_on_both_paths_but_bounds_only_when_taken() {
    for condition in [0, 1] {
        for (high, low) in [(0x5b, 0x80), (0x80, 0x7f)] {
            assert_eq!(
                execute(vec![0x60, condition, 0xe1, high, low, 0], true, 100_000).0,
                InstrStop::InvalidImmediateEncoding
            );
        }
        // +1 would land beyond code. Zero must still fall through to STOP.
        let actual = execute(vec![0x60, condition, 0xe1, 0x80, 0x81, 0], true, 100_000);
        assert_eq!(actual.0, if condition == 0 { InstrStop::Stop } else { InstrStop::InvalidJump });
        // Missing immediate bytes use the existing zero padding safely.
        let truncated = execute(vec![0x60, condition, 0xe1], true, 100_000);
        assert_eq!(
            truncated.0,
            if condition == 0 { InstrStop::Stop } else { InstrStop::InvalidJump }
        );
    }
}

#[test]
fn rjumpi_forward_and_backward_loads_only_taken_targets() {
    for condition in [0, 1] {
        let mut code = vec![0x60, condition];
        push3(&mut code, CODE_CHUNK_SIZE - 5);
        code.push(0x56);
        code.resize(CODE_CHUNK_SIZE - 8, 0);
        code.extend([0x5b, 0, 0]); // Backward destination then STOP.
        code.extend([0x5b, 0xe1, 0x80, 0x81, 0]); // +1 reaches remote JUMPDEST.
        code.extend([0x5b, 0x60, 1, 0xe1, 0x5a, 0x4d]); // -14 returns to boundary -8.
        let actual = execute(code.clone(), true, 100_000);
        assert_eq!(actual.0, InstrStop::Stop);
        assert_eq!(actual.3, if condition == 0 { vec![0] } else { vec![0, 1] });
        assert_eq!(
            actual.2,
            if condition == 0 {
                COLD_CODE_CHUNK_GAS + 19
            } else {
                2 * COLD_CODE_CHUNK_GAS + 1000 + 28
            }
        );
        let tight = execute(code, true, COLD_CODE_CHUNK_GAS + 19);
        assert_eq!(tight.0, if condition == 0 { InstrStop::Stop } else { InstrStop::OutOfGas });
        assert_eq!(tight.3, [0]);
    }
}

#[test]
fn untaken_rjumpi_does_not_warm_a_later_dynamic_jump_target() {
    let mut code = vec![0x5f];
    push3(&mut code, CODE_CHUNK_SIZE - 10);
    code.push(0x56);
    code.resize(CODE_CHUNK_SIZE - 10, 0);
    code.extend([0x5b, 0xe1, 0x80, 0x86]); // +6 would reach the next chunk.
    push3(&mut code, CODE_CHUNK_SIZE);
    code.extend([0x56, 0, 0x5b, 0]);
    let actual = execute(code, true, 100_000);
    assert_eq!(actual.0, InstrStop::Stop);
    assert_eq!(actual.2, 2 * COLD_CODE_CHUNK_GAS + 30);
    assert_eq!(actual.3, [0, 1]);
}

#[test]
fn rjumpi_split_immediates_support_taken_and_fallthrough_paths() {
    for distance in [1, 2] {
        for condition in [0, 1] {
            let mut code = vec![0x60, condition];
            push3(&mut code, CODE_CHUNK_SIZE - distance - 1);
            code.push(0x56);
            code.resize(CODE_CHUNK_SIZE - distance - 1, 0);
            code.extend([0x5b, 0xe1, 0x80, 0x80, 0x5b, 0]);
            let actual = execute(code, true, 100_000);
            assert_eq!(actual.0, InstrStop::Stop);
            // Untaken fallthrough reaches the generated RJUMP; a taken branch skips it.
            assert_eq!(actual.2, 2 * COLD_CODE_CHUNK_GAS + 20 + if condition == 0 { 5 } else { 0 });
            assert_eq!(actual.3, [0, 1]);
        }
    }
}

#[test]
fn rjumpi_backward_loop_is_bounded_by_opcode_gas() {
    let actual = execute(vec![0x5b, 0x60, 1, 0xe1, 0x5a, 0x55], true, COLD_CODE_CHUNK_GAS + 14);
    assert_eq!(actual.0, InstrStop::OutOfGas);
    assert_eq!(actual.2, COLD_CODE_CHUNK_GAS + 14);
}

fn copying_program(source: usize, len: usize, external: bool, repeats: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..repeats {
        push3(&mut code, len);
        push3(&mut code, source);
        code.push(0x5f);
        if external {
            code.extend([0x30, 0x3c]); // ADDRESS, EXTCODECOPY
        } else {
            code.push(0x39);
        }
    }
    push3(&mut code, len);
    code.extend([0x5f, 0xf3]);
    code
}

fn with_moved_values(mut code: Vec<u8>, moved: usize, natural_ae: usize) -> Vec<u8> {
    let boundary = 2 * CODE_CHUNK_SIZE;
    code.resize(boundary - 1, 0);
    code.push(if moved == 0 { 0 } else { 0x5f + moved as u8 });
    code.extend((0..moved).map(|i| 0x20 + i as u8));
    code.extend(vec![0xae; natural_ae]);
    code.extend([0x00; 40]);
    code
}

#[test]
fn both_copies_read_original_prefixes_without_fetching_predecessors() {
    let boundary = 2 * CODE_CHUNK_SIZE;
    for external in [false, true] {
        for (source, len, expected_reads) in [
            (boundary, 32, vec![0, 2]),
            (boundary + 7, 9, vec![0, 2]),
            (boundary - 2, 36, vec![0, 1, 2]),
            (boundary + 32, 4, vec![0, 2]),
            (boundary, 0, vec![0]),
            (boundary + 100, 4, vec![0]),
        ] {
            let raw = with_moved_values(copying_program(source, len, external, 1), 32, 0);
            let reference = execute(raw.clone(), false, 300_000);
            let actual = execute(raw, true, 300_000);
            assert_eq!(actual.0, InstrStop::Return, "external={external}, source={source}");
            assert_eq!(actual.1, reference.1);
            assert_eq!(actual.3, expected_reads);
            if !external && source == boundary - 2 {
                // Only the two intersecting chunks are charged and loaded.
                assert_eq!(actual.2, reference.2 + 3 * COLD_CODE_CHUNK_GAS);
            }
        }
    }
}

#[test]
fn natural_ae_bytes_adjacent_to_leading_data_are_unchanged() {
    let boundary = 2 * CODE_CHUNK_SIZE;
    for external in [false, true] {
        for (moved, natural) in [(0, 8), (3, 8), (32, 8)] {
            for start in [0, moved] {
                let raw = with_moved_values(
                    copying_program(boundary + start, moved + natural + 2 - start, external, 1),
                    moved,
                    natural,
                );
                let expected = raw[boundary + start..boundary + moved + natural + 2].to_vec();
                let result = execute(raw, true, 300_000);
                assert_eq!(result.0, InstrStop::Return);
                assert_eq!(result.1.as_ref(), expected);
                assert_eq!(result.3, [0, 2]);
            }
        }
    }
}

#[test]
fn prefix_copy_charges_only_the_requested_chunk_cold_then_warm() {
    let boundary = 2 * CODE_CHUNK_SIZE;
    let raw = with_moved_values(copying_program(boundary + 5, 7, false, 2), 32, 0);
    let reference = execute(raw.clone(), false, 300_000);
    let actual = execute(raw.clone(), true, 300_000);
    assert_eq!(actual.0, InstrStop::Return);
    assert_eq!(actual.1, reference.1);
    assert_eq!(actual.3, [0, 2]);
    assert_eq!(actual.2, reference.2 + 2 * COLD_CODE_CHUNK_GAS + 1_000);
    let short = execute(raw, true, 2 * COLD_CODE_CHUNK_GAS - 1);
    assert_eq!(short.0, InstrStop::OutOfGas);
    assert_eq!(short.3, [0]);
}

#[test]
fn relative_jumps_reject_leading_push_data_even_when_it_contains_jumpdest() {
    for conditional in [false, true] {
        let mut raw = if conditional { vec![0x60, 1] } else { Vec::new() };
        push3(&mut raw, CODE_CHUNK_SIZE - 10);
        raw.push(0x56);
        raw.resize(CODE_CHUNK_SIZE - 10, 0);
        raw.extend([0x5b, if conditional { 0xe1 } else { 0xe0 }, 0x80, 0x88]);
        raw.resize(CODE_CHUNK_SIZE - 1, 0);
        raw.push(0x7f);
        raw.extend([0x5b; 33]);
        // Relative +8 from boundary-6 reaches JUMPDEST-valued PUSH data.
        let result = execute(raw, true, 300_000);
        assert_eq!(result.0, InstrStop::InvalidJump);
        assert_eq!(result.3, [0, 1]);
    }
}

struct MissingPredecessor(Provider);

impl Database for MissingPredecessor {
    type Error = Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.0.get_account(address)
    }

    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        self.0.get_code_by_hash(hash)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        if index == 1 {
            self.0.reads.borrow_mut().push(index);
            Ok(None)
        } else {
            self.0.get_code_chunk_by_hash(hash, index)
        }
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        self.0.get_storage(address, key)
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.0.get_block_hash(number)
    }
}

#[test]
fn copying_leading_data_succeeds_without_a_stored_predecessor() {
    for external in [false, true] {
        let raw = with_moved_values(copying_program(2 * CODE_CHUNK_SIZE, 7, external, 1), 32, 0);
        let reads = Rc::new(RefCell::new(Vec::new()));
        let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(
                SpecId::PRAGUE,
                Version::new(SpecId::PRAGUE).with_tip1143(true),
            ),
            SpecId::PRAGUE,
            BlockEnvExt::default(),
            TxRegistry::new(),
            Db::new(MissingPredecessor(Provider { raw: raw.into(), reads: reads.clone() })),
            Precompiles::base(SpecId::PRAGUE),
        );
        let result = evm.execute_system_call(
            SystemTx::new(Address::repeat_byte(0x44), Bytes::new()).with_gas_limit(300_000),
        );
        let result = result.unwrap();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.as_ref(), &[0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26]);
        assert_eq!(*reads.borrow(), [0, 2]);
    }
}

#[test]
fn original_relative_jumps_at_payload_end_still_require_jumpdest() {
    for opcode in [0xe0, 0xe1] {
        let mut code = if opcode == 0xe1 { vec![0x60, 1] } else { Vec::new() };
        push3(&mut code, CODE_CHUNK_SIZE - 4);
        code.push(0x56);
        code.resize(CODE_CHUNK_SIZE - 4, 0);
        // Same displacement as the generated tail, but still an original instruction.
        code.extend([0x5b, opcode, 0x5a, 0x38]);
        code.extend([0; 40]);
        let actual = execute(code, true, 200_000);
        assert_eq!(actual.0, InstrStop::InvalidJump);
        assert_eq!(actual.3, [0]);
    }
}
