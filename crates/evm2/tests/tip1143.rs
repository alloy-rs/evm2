//! Sparse runtime acceptance through the public initial-frame execution seam.
//!
//! Draft API: `Version::with_tip1143(bool)` coordinates the feature and limits;
//! `AccountInfo::code_metadata` carries optional validated `CodeMetadata`.

use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::{Bytecode, CodeChunk, CodeMetadata},
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, Database, Db, InterpreterRunner, SystemTx},
    interpreter::{GasTracker, InstrStop, Interpreter, Word},
    registry::TxRegistry,
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const CHUNK: usize = 24541;
const COLD: u64 = 28680;
const WARM: u64 = 1000;

#[derive(Clone, Debug)]
struct Provider {
    code: Bytes,
    hash: B256,
    reads: Rc<RefCell<Vec<u32>>>,
    full_reads: Rc<RefCell<usize>>,
}

impl Database for Provider {
    type Error = io::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        if *address != Address::repeat_byte(0x44) {
            return Ok(None);
        }
        Ok(Some(AccountInfo {
            nonce: 1,
            code_hash: self.hash,
            code_metadata: if self.code.len() > CHUNK {
                Some(
                    CodeMetadata::new(
                        self.code.len() as u32,
                        self.code.chunks(CHUNK).map(keccak256).collect(),
                    )
                    .unwrap(),
                )
            } else {
                None
            },
            ..Default::default()
        }))
    }

    fn get_code_kind_by_hash(
        &mut self,
        _hash: &B256,
    ) -> Result<evm2::bytecode::BytecodeKind, Self::Error> {
        Ok(evm2::bytecode::BytecodeKind::Legacy)
    }

    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        assert_eq!(*hash, self.hash);
        *self.full_reads.borrow_mut() += 1;
        self.reads.borrow_mut().push(0);
        assert!(self.code.len() <= CHUNK, "execution reconstructed multi-chunk code");
        Ok(Bytecode::new_legacy(self.code.clone()))
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        assert_eq!(*hash, self.hash);
        self.reads.borrow_mut().push(index);
        Ok(evm2::bytecode::code_chunk(&self.code, index))
    }
}

fn execute(code: Vec<u8>, enabled: bool, budget: u64) -> (InstrStop, Bytes, u64, Vec<u32>, usize) {
    let provider = Provider {
        hash: keccak256(&code),
        code: code.into(),
        reads: Rc::default(),
        full_reads: Rc::default(),
    };
    let reads = provider.reads.clone();
    let full_reads = provider.full_reads.clone();
    let version = Version::new(SpecId::PRAGUE).with_tip1143(enabled);
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(provider),
        Precompiles::base(SpecId::PRAGUE),
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
    let observed = reads.borrow().clone();
    let full = *full_reads.borrow();
    (result.stop, result.output, gas.spent(), observed, full)
}

fn push3(code: &mut Vec<u8>, value: usize) {
    assert!(value < 1 << 24);
    code.extend([0x62, (value >> 16) as u8, (value >> 8) as u8, value as u8]);
}

#[test]
fn tip1143_t01_disabled_legacy_gas_golden() {
    // PUSH1 42; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN.
    let code = vec![0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
    let disabled = execute(code.clone(), false, 100_000);
    assert_eq!(disabled.0, InstrStop::Return);
    assert_eq!(disabled.2, 18);
    assert_eq!(disabled.3, vec![0]);
    assert_eq!(disabled.4, 1);
    let enabled = execute(code, true, 100_000);
    assert_eq!(enabled.0, disabled.0);
    assert_eq!(enabled.1, disabled.1);
    assert_eq!(enabled.2, 18 + COLD);
    assert_eq!(enabled.3, [0]);
    for (fork, runtime, initcode) in
        [(SpecId::PRAGUE, 24576, 49152), (SpecId::AMSTERDAM, 65536, 131072)]
    {
        let default = Version::new(fork);
        assert_eq!((default.max_code_size, default.max_initcode_size), (runtime, initcode));
        let enabled = default.with_tip1143(true);
        assert_eq!((enabled.max_code_size, enabled.max_initcode_size), (981640, 1966080));
    }
}

#[test]
fn tip1143_t12_sparse_short_fortieth_chunk() {
    let target = 39 * CHUNK;
    let mut code = Vec::new();
    push3(&mut code, target);
    code.push(0x56);
    code.resize(target + 1, 0);
    code[target] = 0x5b;
    let result = execute(code, true, 100_000);
    assert_eq!(result.0, InstrStop::Stop);
    assert_eq!(result.2, 2 * COLD + 3 + 8 + 1);
    assert_eq!(result.3, [0, 39]);
    assert_eq!(result.4, 0);
}

#[test]
fn tip1143_t15_remote_return_jump_preserves_memory() {
    let target = 2 * CHUNK;
    let mut code = vec![0x60, 42, 0x60, 0, 0x52];
    push3(&mut code, target);
    code.push(0x56);
    let back = code.len();
    code.extend([0x5b, 0x60, 32, 0x60, 0, 0xf3]);
    code.resize(target, 0);
    code.push(0x5b);
    push3(&mut code, back);
    code.push(0x56);
    let result = execute(code, true, 100_000);
    let mut expected = vec![0; 32];
    expected[31] = 42;
    assert_eq!(result.0, InstrStop::Return);
    assert_eq!(result.1.as_ref(), expected);
    assert_eq!(result.2, 2 * COLD + WARM + 42);
    assert_eq!(result.3, [0, 2]);
}

#[test]
fn tip1143_t15_untaken_maximal_target_does_not_load() {
    let mut code = vec![0x60, 0, 0x7f];
    code.extend([0xff; 32]);
    code.extend([0x57, 0]);
    code.resize(3 * CHUNK, 0);
    let result = execute(code, true, 100_000);
    assert_eq!(result.0, InstrStop::Stop);
    assert_eq!(result.2, COLD + 16);
    assert_eq!(result.3, [0]);
}

#[test]
fn tip1143_t16_remote_invalid_destination_reads_but_out_of_range_does_not() {
    for (target, reads) in [(CHUNK, vec![0, 1]), (CHUNK + 1, vec![0]), (981640, vec![0])] {
        let mut code = Vec::new();
        push3(&mut code, target);
        code.push(0x56);
        code.resize(CHUNK + 1, 0);
        let result = execute(code, true, 100_000);
        assert_eq!(result.0, InstrStop::InvalidJump);
        assert_eq!(result.3, reads, "target={target}");
    }
}

#[test]
fn tip1143_t17_global_pc_and_codesize() {
    let mut code = Vec::new();
    push3(&mut code, CHUNK);
    code.push(0x56);
    code.resize(CHUNK, 0);
    code.extend([0x5b, 0x58, 0x60, 0, 0x52, 0x38, 0x60, 32, 0x52, 0x60, 64, 0x60, 0, 0xf3]);
    let size = code.len();
    let result = execute(code, true, 100_000);
    assert_eq!(result.0, InstrStop::Return);
    assert_eq!(U256::from_be_slice(&result.1[..32]), U256::from(CHUNK + 1));
    assert_eq!(U256::from_be_slice(&result.1[32..]), U256::from(size));
    assert_eq!(result.3, [0, 1]);
}

#[test]
fn tip1143_t18_codecopy_boundary_and_zero_tail() {
    for (source, length) in
        [(0, 0), (CHUNK - 1, 1), (CHUNK - 1, 2), (CHUNK, 1), (CHUNK + 2, 4), (CHUNK + 3, 8)]
    {
        let mut code = Vec::new();
        push3(&mut code, length);
        push3(&mut code, source);
        code.extend([0x60, 0, 0x39]);
        push3(&mut code, length);
        code.extend([0x60, 0, 0xf3]);
        code.resize(CHUNK, 0);
        code.extend([0x5b, 0x30, 0]);
        let expected = (source..source + length)
            .map(|i| code.get(i).copied().unwrap_or(0))
            .collect::<Vec<_>>();
        let result = execute(code, true, 200_000);
        assert_eq!(result.0, InstrStop::Return);
        assert_eq!(result.1.as_ref(), expected, "source={source}, length={length}");
        let touches_tail = length > 0 && source < CHUNK + 3 && source + length > CHUNK;
        assert_eq!(result.3, if touches_tail { vec![0, 1] } else { vec![0] });
    }
}

#[test]
fn tip1143_t22_entry_reservation_precedes_provider_read() {
    for budget in [COLD - 1, COLD, COLD + 1] {
        let result = execute(vec![0], true, budget);
        if budget < COLD {
            assert_eq!(result.0, InstrStop::OutOfGas);
            assert!(result.3.is_empty());
        } else {
            assert_eq!(result.0, InstrStop::Stop);
            assert_eq!(result.2, COLD);
            assert_eq!(result.3, [0]);
        }
    }
}

fn push_word(code: &mut Vec<u8>, value: U256) {
    code.push(0x7f);
    code.extend(value.to_be_bytes::<32>());
}

#[test]
fn tip1143_t19_codecopy_wide_operands_do_not_alias_low_offsets() {
    let high_alias = (U256::from(1) << 128) + U256::from(CHUNK);
    for source in [U256::MAX, high_alias] {
        let mut code = Vec::new();
        push3(&mut code, 8);
        push_word(&mut code, source);
        code.extend([0x60, 0, 0x39, 0x60, 8, 0x60, 0, 0xf3]);
        code.resize(CHUNK + 1, 0);
        code[CHUNK] = 0x5b;
        let result = execute(code, true, 100_000);
        assert_eq!(result.0, InstrStop::Return);
        assert_eq!(result.1.as_ref(), &[0; 8]);
        assert_eq!(result.2, COLD + 24);
        assert_eq!(result.3, [0]);
    }
    for source in [U256::ZERO, U256::MAX, high_alias] {
        let mut code = vec![0x60, 0];
        push_word(&mut code, source);
        push_word(&mut code, U256::MAX);
        code.extend([0x39, 0]);
        code.resize(CHUNK + 1, 0);
        let result = execute(code, true, 100_000);
        assert_eq!(result.0, InstrStop::Stop);
        assert_eq!(result.2, COLD + 12);
        assert_eq!(result.3, [0]);
    }
    for (length, destination) in [(U256::MAX, U256::ZERO), (U256::from(1), U256::MAX)] {
        let mut code = Vec::new();
        push_word(&mut code, length);
        push3(&mut code, CHUNK);
        push_word(&mut code, destination);
        code.extend([0x39, 0]);
        code.resize(CHUNK + 1, 0);
        let result = execute(code, true, 100_000);
        assert!(!result.0.is_success());
        assert_eq!(result.2, 100_000);
        assert_eq!(result.3, [0]);
    }
}

#[test]
fn tip1143_t22_copy_aggregate_reservation_precedes_first_remote_read() {
    // Active chunk zero is warm; chunks one/two are cold. The copy requests all
    // three exactly once. End with STOP so the bracket isolates this operation.
    let length = 2 * CHUNK + 1;
    let mut code = Vec::new();
    push3(&mut code, length);
    code.extend([0x60, 0, 0x60, 0, 0x39, 0]);
    code.resize(length, 0);
    let words = (length as u64).div_ceil(32);
    let ordinary = 12 + 3 * words + 3 * words + words * words / 512;
    let total = COLD + ordinary + WARM + 2 * COLD;
    for budget in [total - 1, total, total + 1] {
        let result = execute(code.clone(), true, budget);
        if budget < total {
            assert_eq!(result.0, InstrStop::OutOfGas);
            assert_eq!(result.3, [0], "aggregate OOG fetched a remote chunk");
        } else {
            assert_eq!(result.0, InstrStop::Stop);
            assert_eq!(result.2, total);
            assert_eq!(result.3, [0, 1, 2]);
        }
    }
}

#[test]
fn tip1143_t22_remote_jump_budget_bracket_including_invalid_destination() {
    for destination_opcode in [0x5b, 0x30] {
        let mut code = Vec::new();
        push3(&mut code, CHUNK);
        code.push(0x56);
        code.resize(CHUNK + 2, 0);
        code[CHUNK] = destination_opcode;
        let required = 2 * COLD + 3 + 8;
        let unaffordable = execute(code.clone(), true, required - 1);
        assert_eq!(unaffordable.0, InstrStop::OutOfGas);
        assert_eq!(unaffordable.3, [0]);
        let affordable = execute(code, true, required + 1);
        assert_eq!(affordable.3, [0, 1]);
        if destination_opcode == 0x5b {
            assert_eq!(affordable.0, InstrStop::Stop);
            assert_eq!(affordable.2, required + 1);
        } else {
            assert_eq!(affordable.0, InstrStop::InvalidJump);
        }
    }
}

#[test]
fn tip1143_t17_reached_boundary_stop_does_not_fall_through() {
    let mut code = Vec::new();
    push3(&mut code, CHUNK - 2);
    code.push(0x56);
    code.resize(CHUNK + 1, 0);
    code[CHUNK - 2] = 0x5b;
    code[CHUNK] = 0xfe;
    let result = execute(code, true, 100_000);
    assert_eq!(result.0, InstrStop::Stop);
    assert_eq!(result.2, COLD + 12);
    assert_eq!(result.3, [0]);
}

/// Separate original account records and payload reads for multi-owner programs.
#[derive(Clone, Debug, Default)]
struct World {
    accounts: BTreeMap<Address, Bytes>,
    reads: Rc<RefCell<Vec<(B256, u32)>>>,
}

impl Database for World {
    type Error = io::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self.accounts.get(address).map(|code| AccountInfo {
            nonce: 1,
            code_hash: keccak256(code),
            code_metadata: (code.len() > CHUNK).then(|| {
                CodeMetadata::new(code.len() as u32, code.chunks(CHUNK).map(keccak256).collect())
                    .unwrap()
            }),
            ..Default::default()
        }))
    }

    fn get_code_kind_by_hash(
        &mut self,
        _hash: &B256,
    ) -> Result<evm2::bytecode::BytecodeKind, Self::Error> {
        Ok(evm2::bytecode::BytecodeKind::Legacy)
    }

    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        let code = self.accounts.values().find(|code| keccak256(code) == *hash).unwrap();
        assert!(code.len() <= CHUNK, "unexpected complete multi-code lookup");
        self.reads.borrow_mut().push((*hash, 0));
        Ok(Bytecode::new_legacy(code.clone()))
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        self.reads.borrow_mut().push((*hash, index));
        Ok(self
            .accounts
            .values()
            .find(|code| keccak256(code) == *hash)
            .and_then(|code| evm2::bytecode::code_chunk(code, index)))
    }
}

fn world_evm(world: World, enabled: bool) -> Evm<'static, BaseEvmTypes> {
    Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(
            SpecId::PRAGUE,
            Version::new(SpecId::PRAGUE).with_tip1143(enabled),
        ),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(world),
        Precompiles::base(SpecId::PRAGUE),
    )
}

fn push_address(code: &mut Vec<u8>, address: Address) {
    code.push(0x73);
    code.extend(address.as_slice());
}

#[test]
fn tip1143_t20_legacy_extcodecopy_unknown_then_cached_size() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    let original = Bytes::from(vec![0xab; 17]);
    for source in [
        U256::ZERO,
        U256::from(16),
        U256::from(17),
        U256::from(CHUNK - 1),
        U256::from(CHUNK),
        U256::MAX,
    ] {
        for length in [0, 4] {
            let mut code = Vec::new();
            // Second operation uses already resolved size and warm bytes, when the
            // first operation's fixed legacy predicate required a lookup.
            for _ in 0..2 {
                push3(&mut code, length);
                push_word(&mut code, source);
                code.extend([0x60, 0]);
                push_address(&mut code, target);
                code.push(0x3c);
            }
            push3(&mut code, length);
            code.extend([0x60, 0, 0xf3]);
            let caller_hash = keccak256(&code);
            let target_hash = keccak256(&original);
            let mut world = World::default();
            world.accounts.insert(caller, code.into());
            world.accounts.insert(target, original.clone());
            let reads = world.reads.clone();
            let mut execution = world_evm(world, true);
            let result =
                execution.system_call(SystemTx::new(caller, Bytes::new())).unwrap().discard();
            assert_eq!(result.stop, InstrStop::Return);
            let expected = (0..length)
                .map(|i| {
                    if source < U256::from(17) {
                        original.get(source.to::<usize>() + i).copied().unwrap_or(0)
                    } else {
                        0
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(result.output.as_ref(), expected, "source={source}, length={length}");
            let requested = length != 0 && source < U256::from(24576);
            // Two sets of four PUSHes, cold/warm account access, RETURN's
            // two PUSHes, two word copies, and one memory expansion.
            let ordinary = 24 + 2600 + 100 + 6 + if length == 0 { 0 } else { 6 + 3 };
            assert_eq!(
                result.total_gas_spent,
                COLD + ordinary + if requested { COLD + WARM } else { 0 }
            );
            let expected_reads = if requested {
                vec![(caller_hash, 0), (target_hash, 0)]
            } else {
                vec![(caller_hash, 0)]
            };
            assert_eq!(*reads.borrow(), expected_reads);
        }
    }
}

#[test]
fn tip1143_t21_external_size_and_hash_obey_representation() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    for payload in [
        None,
        Some(Bytes::new()),
        Some(Bytes::from(vec![0; 17])),
        Some(Bytes::from(vec![0; CHUNK + 1])),
    ] {
        let mut code = Vec::new();
        for (offset, opcode) in [(0, 0x3b), (32, 0x3b), (64, 0x3f)] {
            push_address(&mut code, target);
            code.extend([opcode, 0x60, offset, 0x52]);
        }
        code.extend([0x60, 96, 0x60, 0, 0xf3]);
        let caller_hash = keccak256(&code);
        let mut world = World::default();
        world.accounts.insert(caller, code.into());
        if let Some(bytes) = &payload {
            world.accounts.insert(target, bytes.clone());
        }
        let reads = world.reads.clone();
        let mut execution = world_evm(world, true);
        let result = execution.system_call(SystemTx::new(caller, Bytes::new())).unwrap().discard();
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.len(), 96);
        let size = payload.as_ref().map_or(0, |bytes| bytes.len());
        for offset in [0, 32] {
            assert_eq!(U256::from_be_slice(&result.output[offset..offset + 32]), U256::from(size));
        }
        let hash = payload.as_ref().map_or(B256::ZERO, keccak256);
        assert_eq!(&result.output[64..], hash.as_slice());
        let legacy = size > 0 && size <= CHUNK;
        assert_eq!(result.total_gas_spent, COLD + 2842 + if legacy { COLD + WARM } else { 0 });
        assert_eq!(
            *reads.borrow(),
            if legacy { vec![(caller_hash, 0), (hash, 0)] } else { vec![(caller_hash, 0)] }
        );
    }
}

#[derive(Debug)]
struct RunnerSpy(Arc<AtomicUsize>);

impl InterpreterRunner<BaseEvmTypes> for RunnerSpy {
    fn run<'frame, 'host>(
        &self,
        _: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        _: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        self.0.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[test]
fn tip1143_t32_uninspected_runner_has_disabled_positive_control() {
    for enabled in [false, true] {
        for multi in [false, true] {
            if multi && !enabled {
                continue;
            }
            let caller = Address::repeat_byte(0x44);
            let target = Address::repeat_byte(0x77);
            let mut code = Vec::new();
            push_address(&mut code, target);
            code.extend([0x3b, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
            if multi {
                code.resize(CHUNK + 1, 0);
            }
            let mut world = World::default();
            world.accounts.insert(caller, code.into());
            world.accounts.insert(target, Bytes::from_static(&[0]));
            let mut execution = world_evm(world, enabled);
            let calls = Arc::new(AtomicUsize::new(0));
            execution.set_interpreter_runner(RunnerSpy(calls.clone()));
            let result =
                execution.system_call(SystemTx::new(caller, Bytes::new())).unwrap().discard();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(U256::from_be_slice(&result.output), U256::from(1));
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(!enabled));
            assert_eq!(result.total_gas_spent, 2618 + if enabled { 2 * COLD } else { 0 });
        }
    }
}

#[test]
fn tip1143_t39_remote_parent_resumes_after_child_success_and_revert() {
    for revert in [false, true] {
        let parent = Address::repeat_byte(0x44);
        let child = Address::repeat_byte(0x77);
        let mut parent_code = Vec::new();
        push3(&mut parent_code, CHUNK);
        parent_code.push(0x56);
        parent_code.resize(CHUNK, 0);
        // Stack sentinel and memory sentinel must survive the child frame.
        parent_code.extend([0x5b, 0x60, 0x51, 0x60, 0xa1, 0x60, 0, 0x52]);
        // CALL(gas=200000, child, value=0, input=empty, output=[32,64)).
        parent_code.extend([0x60, 32, 0x60, 32, 0x60, 0, 0x60, 0, 0x60, 0]);
        push_address(&mut parent_code, child);
        push3(&mut parent_code, 200000);
        parent_code.push(0xf1);
        // Store success, surviving stack sentinel, resumed PC, and return size.
        parent_code.extend([0x60, 64, 0x52, 0x60, 96, 0x52]);
        let resumed_pc = parent_code.len();
        parent_code
            .extend([0x58, 0x60, 128, 0x52, 0x3d, 0x60, 160, 0x52, 0x60, 192, 0x60, 0, 0xf3]);
        let mut child_code = Vec::new();
        push3(&mut child_code, 2 * CHUNK);
        child_code.push(0x56);
        child_code.resize(2 * CHUNK, 0);
        child_code.extend([
            0x5b,
            0x60,
            0xb2,
            0x60,
            0,
            0x52,
            0x60,
            32,
            0x60,
            0,
            if revert { 0xfd } else { 0xf3 },
        ]);
        let parent_hash = keccak256(&parent_code);
        let child_hash = keccak256(&child_code);
        let mut world = World::default();
        world.accounts.insert(parent, parent_code.into());
        world.accounts.insert(child, child_code.into());
        let reads = world.reads.clone();
        let mut execution = world_evm(world, true);
        for _ in 0..2 {
            // Repeated execution also exercises the interpreter frame pool.
            let result =
                execution.system_call(SystemTx::new(parent, Bytes::new())).unwrap().discard();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.output.len(), 192);
            let expected = [
                U256::from(0xa1),
                U256::from(0xb2),
                U256::from(u8::from(!revert)),
                U256::from(0x51),
                U256::from(resumed_pc),
                U256::from(32),
            ];
            for (word, expected) in result.output.as_chunks::<32>().0.iter().zip(expected) {
                assert_eq!(U256::from_be_slice(word), expected, "revert={revert}");
            }
            assert!(result.logs.is_empty());
        }
        assert_eq!(
            *reads.borrow(),
            [(parent_hash, 0), (parent_hash, 1), (child_hash, 0), (child_hash, 2)]
        );
    }
}

#[test]
fn tip1143_t23_entry_tariff_precedes_eip150_forwarding() {
    let parent = Address::repeat_byte(0x44);
    let child = Address::repeat_byte(0x77);
    for multi in [false, true] {
        for requested in [50_000, 1_000_000] {
            // Sweep all residues around the 63/64 rounding boundary.
            for residue in 0..64 {
                let mut parent_code = vec![0x60, 32, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0];
                push_address(&mut parent_code, child);
                push3(&mut parent_code, requested);
                parent_code.extend([0xf1, 0x50, 0x60, 32, 0x60, 0, 0xf3]);
                let mut child_code = vec![0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
                if multi {
                    child_code.resize(CHUNK + 1, 0);
                }
                let parent_hash = keccak256(&parent_code);
                let child_hash = keccak256(&child_code);
                let mut world = World::default();
                world.accounts.insert(parent, parent_code.into());
                world.accounts.insert(child, child_code.into());
                let reads = world.reads.clone();
                let mut execution = world_evm(world, true);
                for warm in [false, true] {
                    let budget = 250_000 + residue;
                    let entry = if warm { WARM } else { COLD };
                    let account = if warm { 100 } else { 2600 };
                    // A excludes seven PUSHes, output-memory expansion, and
                    // ordinary account access. The child entry tariff occurs
                    // exactly once, before applying EIP-150.
                    let a = budget - entry - 21 - 3 - account;
                    let after_tariff = a - entry;
                    let forwarded = (requested as u64).min(after_tariff - after_tariff / 64);
                    let result = execution
                        .execute_system_call(
                            SystemTx::new(parent, Bytes::new()).with_gas_limit(budget),
                        )
                        .unwrap();
                    assert_eq!(result.stop, InstrStop::Return);
                    assert_eq!(result.output.len(), 32);
                    assert_eq!(
                        U256::from_be_slice(&result.output),
                        U256::from(forwarded - 2),
                        "multi={multi}, requested={requested}, residue={residue}, warm={warm}"
                    );
                    // Child consumes 17 ordinary gas; parent consumes POP and
                    // the two RETURN arguments after CALL.
                    assert_eq!(result.total_gas_spent, 2 * entry + account + 21 + 3 + 17 + 2 + 6);
                }
                assert_eq!(*reads.borrow(), [(parent_hash, 0), (child_hash, 0)]);
            }
        }
    }
}

#[test]
fn tip1143_t18_multi_extcodecopy_clips_before_loading() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    let mut payload = vec![0; 39 * CHUNK + 1];
    // Distinct legal chunks, all non-final chunks end with decoded STOP.
    for index in 0..40 {
        payload[index * CHUNK] = 0x5b;
        if index < 39 {
            payload[index * CHUNK + 1] = 0x30;
        }
    }
    let size = payload.len();
    let target_hash = keccak256(&payload);
    for (source, length) in [
        (U256::ZERO, 0),
        (U256::from(size), 1),
        (U256::from(size + 1), 8),
        (U256::MAX, 8),
        ((U256::from(1) << 128) + U256::from(CHUNK), 8),
        (U256::from(CHUNK - 1), 1),
        (U256::from(CHUNK - 1), 2),
        (U256::from(CHUNK), 1),
        (U256::from(size - 1), 4),
        (U256::ZERO, size),
    ] {
        let mut code = Vec::new();
        push3(&mut code, length);
        push_word(&mut code, source);
        code.extend([0x60, 0]);
        push_address(&mut code, target);
        code.push(0x3c);
        push3(&mut code, length);
        code.extend([0x60, 0, 0xf3]);
        let caller_hash = keccak256(&code);
        let mut world = World::default();
        world.accounts.insert(caller, code.into());
        world.accounts.insert(target, Bytes::copy_from_slice(&payload));
        let reads = world.reads.clone();
        let mut execution = world_evm(world, true);
        let result = execution
            .system_call(SystemTx::new(caller, Bytes::new()).with_gas_limit(100_000_000))
            .unwrap()
            .discard();
        assert_eq!(result.stop, InstrStop::Return);
        let mut expected = vec![0; length];
        let mut expected_reads = vec![(caller_hash, 0)];
        if length > 0 && source < U256::from(size) {
            let start = source.to::<usize>();
            let copied = length.min(size - start);
            expected[..copied].copy_from_slice(&payload[start..start + copied]);
            for index in start / CHUNK..=(start + copied - 1) / CHUNK {
                expected_reads.push((target_hash, index as u32));
            }
        }
        assert_eq!(result.output.as_ref(), expected, "source={source}, length={length}");
        assert_eq!(*reads.borrow(), expected_reads);
        let words = (length as u64).div_ceil(32);
        let ordinary = 12 + 2600 + 3 * words + 3 * words + words * words / 512 + 6;
        assert_eq!(result.total_gas_spent, ordinary + COLD * expected_reads.len() as u64);
    }
}

#[test]
fn tip1143_t13_empty_nonexistent_and_precompile_entry_skip_runtime_reads() {
    let absent = Address::repeat_byte(0x41);
    let empty = Address::repeat_byte(0x42);
    let legacy = Address::repeat_byte(0x43);
    let multi = Address::repeat_byte(0x44);
    let identity = Address::with_last_byte(4);
    let legacy_code = Bytes::from_static(&[0]);
    let multi_code = Bytes::from(vec![0; CHUNK + 1]);
    let mut world = World::default();
    world.accounts.insert(empty, Bytes::new());
    world.accounts.insert(legacy, legacy_code.clone());
    world.accounts.insert(multi, multi_code.clone());
    // A code row at a dispatched precompile must not cause runtime loading.
    world.accounts.insert(identity, Bytes::from_static(&[0xfe]));
    let reads = world.reads.clone();
    let mut execution = world_evm(world, true);
    for owner in [absent, empty, identity] {
        for _ in 0..2 {
            let input = Bytes::from_static(&[0x12, 0x34]);
            let result =
                execution.execute_system_call(SystemTx::new(owner, input.clone())).unwrap();
            assert!(result.stop.is_success());
            if owner == identity {
                assert_eq!(result.output, input);
                assert_eq!(result.total_gas_spent, 18);
            } else {
                assert!(result.output.is_empty());
                assert_eq!(result.total_gas_spent, 0);
            }
            assert!(result.logs.is_empty());
            assert!(reads.borrow().is_empty());
        }
    }
    for owner in [legacy, multi] {
        // Account metadata was already loaded before executing runtime.
        assert!(execution.state_mut().account(&owner).unwrap().code_chunks().is_empty());
        for tariff in [COLD, WARM] {
            let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
            assert_eq!(result.stop, InstrStop::Stop);
            assert_eq!(result.total_gas_spent, tariff);
            assert!(result.output.is_empty());
        }
        let account = execution.state_mut().account(&owner).unwrap();
        assert_eq!(account.code_chunks().len(), 1);
        assert!(account.code_chunks().get(&0).unwrap().is_warm);
        assert_eq!(account.code_size(), Some(if owner == legacy { 1 } else { (CHUNK + 1) as u32 }));
    }
    assert_eq!(*reads.borrow(), [(keccak256(&legacy_code), 0), (keccak256(&multi_code), 0)]);
}

#[test]
fn tip1143_t19_external_copy_wide_memory_operands_precede_chunk_io() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    for size in [17, CHUNK + 1] {
        for (length, source, destination, success) in [
            (U256::ZERO, U256::MAX, U256::MAX, true),
            (U256::MAX, U256::ZERO, U256::ZERO, false),
            (U256::from(1), U256::ZERO, U256::MAX, false),
            (U256::from(4), U256::MAX, U256::ZERO, true),
            (U256::from(4), (U256::from(1) << 128) + U256::from(1), U256::ZERO, true),
        ] {
            let mut code = Vec::new();
            push_word(&mut code, length);
            push_word(&mut code, source);
            push_word(&mut code, destination);
            push_address(&mut code, target);
            code.extend([0x3c, 0]);
            let hash = keccak256(&code);
            let mut world = World::default();
            world.accounts.insert(caller, code.into());
            world.accounts.insert(target, vec![0; size].into());
            let reads = world.reads.clone();
            let mut execution = world_evm(world, true);
            let result = execution
                .execute_system_call(SystemTx::new(caller, Bytes::new()).with_gas_limit(100_000))
                .unwrap();
            assert_eq!(
                result.stop.is_success(),
                success,
                "size={size}, length={length}, destination={destination}"
            );
            assert_eq!(*reads.borrow(), [(hash, 0)]);
            assert!(execution.state_mut().account(&target).unwrap().code_chunks().is_empty());
            let stats = execution.state_mut().code_chunk_stats();
            assert_eq!(stats.logical_cold_accesses, 1);
            assert_eq!(stats.logical_warm_accesses, 0);
            if success {
                assert_eq!(result.stop, InstrStop::Stop);
                assert_eq!(
                    result.total_gas_spent,
                    COLD + 12 + 2600 + if length == U256::ZERO { 0 } else { 6 }
                );
            } else {
                assert_eq!(result.total_gas_spent, 100_000);
            }
        }
    }
}

#[test]
fn tip1143_t22_external_copy_reserves_all_cold_and_mixed_tariffs_atomically() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    let length = 2 * CHUNK + 1;
    let payload = Bytes::from(vec![0; length]);
    let target_hash = keccak256(&payload);
    for prewarm_zero in [false, true] {
        let words = (length as u64).div_ceil(32);
        let ordinary = 12
            + if prewarm_zero { 100 } else { 2600 }
            + 3 * words
            + 3 * words
            + words * words / 512;
        let tariff = if prewarm_zero { WARM + 2 * COLD } else { 3 * COLD };
        let total = COLD + ordinary + tariff;
        for budget in [total - 1, total, total + 1] {
            let mut code = Vec::new();
            push3(&mut code, length);
            code.extend([0x60, 0, 0x60, 0]);
            push_address(&mut code, target);
            code.extend([0x3c, 0]);
            let caller_hash = keccak256(&code);
            let mut world = World::default();
            world.accounts.insert(caller, code.into());
            world.accounts.insert(target, payload.clone());
            let reads = world.reads.clone();
            let mut execution = world_evm(world, true);
            if prewarm_zero {
                let warmup =
                    execution.execute_system_call(SystemTx::new(target, Bytes::new())).unwrap();
                assert_eq!(warmup.stop, InstrStop::Stop);
                assert_eq!(warmup.total_gas_spent, COLD);
            }
            let before = execution.state_mut().code_chunk_stats();
            let result = execution
                .execute_system_call(SystemTx::new(caller, Bytes::new()).with_gas_limit(budget))
                .unwrap();
            let after = execution.state_mut().code_chunk_stats();
            let mut expected = if prewarm_zero { vec![(target_hash, 0)] } else { vec![] };
            expected.push((caller_hash, 0));
            if budget < total {
                assert_eq!(result.stop, InstrStop::OutOfGas);
                assert_eq!(after.logical_cold_accesses - before.logical_cold_accesses, 1);
                assert_eq!(after.logical_warm_accesses - before.logical_warm_accesses, 0);
            } else {
                assert_eq!(result.stop, InstrStop::Stop);
                assert_eq!(result.total_gas_spent, total);
                expected.extend((u32::from(prewarm_zero)..3).map(|index| (target_hash, index)));
                assert_eq!(
                    after.logical_cold_accesses - before.logical_cold_accesses,
                    if prewarm_zero { 3 } else { 4 }
                );
                assert_eq!(
                    after.logical_warm_accesses - before.logical_warm_accesses,
                    u64::from(prewarm_zero)
                );
            }
            assert_eq!(*reads.borrow(), expected);
        }
    }
}

#[test]
fn tip1143_t20_rollback_cached_legacy_copy_preserves_cold_logical_tariff() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    for source in [0, 16, 17, CHUNK - 1, CHUNK] {
        for length in [0, 4] {
            let mut code = Vec::new();
            push3(&mut code, length);
            push3(&mut code, source);
            code.extend([0x60, 0]);
            push_address(&mut code, target);
            code.push(0x3c);
            push3(&mut code, length);
            code.extend([0x60, 0, 0xf3]);
            let caller_hash = keccak256(&code);
            let payload = Bytes::from(vec![0xab; 17]);
            let target_hash = keccak256(&payload);
            let mut world = World::default();
            world.accounts.insert(caller, code.into());
            world.accounts.insert(target, payload);
            let reads = world.reads.clone();
            let mut execution = world_evm(world, true);
            let features = execution.version().features;
            let checkpoint = execution.state_mut().checkpoint();
            let first = execution.execute_system_call(SystemTx::new(caller, Bytes::new())).unwrap();
            let required = length > 0 && source < 24576;
            if required {
                assert_eq!(execution.state_mut().account(&target).unwrap().code_size(), Some(17));
            }
            execution.state_mut().rollback(checkpoint, features);
            let second =
                execution.execute_system_call(SystemTx::new(caller, Bytes::new())).unwrap();
            assert_eq!(first.stop, InstrStop::Return);
            assert_eq!(second.stop, first.stop);
            assert_eq!(second.output, first.output);
            assert_eq!(second.total_gas_spent, first.total_gas_spent);
            assert_eq!(
                first.total_gas_spent,
                COLD + 18
                    + 2600
                    + if length == 0 { 0 } else { 6 }
                    + if required { COLD } else { 0 }
            );
            let expected = if required {
                vec![(caller_hash, 0), (target_hash, 0)]
            } else {
                vec![(caller_hash, 0)]
            };
            assert_eq!(*reads.borrow(), expected);
        }
    }
}

#[test]
fn tip1143_t17_concrete_geth_trace_reports_global_pc_and_remote_push() {
    let mut code = Vec::new();
    push3(&mut code, CHUNK);
    code.push(0x56);
    code.resize(CHUNK, 0);
    code.extend([0x5b, 0x61, 0x12, 0x34, 0x50, 0x58, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    let provider = Provider {
        hash: keccak256(&code),
        code: code.into(),
        reads: Rc::default(),
        full_reads: Rc::default(),
    };
    let reads = provider.reads.clone();
    let full_reads = provider.full_reads.clone();
    let mut inspector = TracingInspector::new(TracingInspectorConfig::default_geth());
    let result = {
        let mut execution = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
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
        execution.set_inspector(&mut inspector);
        execution
            .execute_system_call(SystemTx::new(Address::repeat_byte(0x44), Bytes::new()))
            .unwrap()
    };
    assert_eq!(result.stop, InstrStop::Return);
    assert_eq!(U256::from_be_slice(&result.output), U256::from(CHUNK + 5));
    let trace = inspector.geth_builder().geth_traces(
        result.total_gas_spent,
        result.output,
        Default::default(),
    );
    assert_eq!(
        trace.struct_logs.iter().map(|step| step.pc).collect::<Vec<_>>(),
        [
            0,
            4,
            CHUNK as u64,
            CHUNK as u64 + 1,
            CHUNK as u64 + 4,
            CHUNK as u64 + 5,
            CHUNK as u64 + 6,
            CHUNK as u64 + 8,
            CHUNK as u64 + 9,
            CHUNK as u64 + 11,
            CHUNK as u64 + 13
        ]
    );
    assert_eq!(trace.struct_logs[3].op, "PUSH2");
    assert_eq!(trace.struct_logs[4].stack.as_ref().unwrap().last(), Some(&U256::from(0x1234)));
    assert_eq!(*reads.borrow(), [0, 1]);
    assert_eq!(*full_reads.borrow(), 0);
}

#[test]
fn tip1143_t01_default_and_disabled_fork_limits_and_execution_goldens() {
    for (spec, old_runtime, old_initcode) in
        [(SpecId::PRAGUE, 24576, 49152), (SpecId::AMSTERDAM, 65536, 131072)]
    {
        for explicit in [false, true] {
            let base = Version::new(spec);
            let version = if explicit { base.with_tip1143(false) } else { base };
            assert_eq!(version.max_code_size, old_runtime);
            assert_eq!(version.max_initcode_size, old_initcode);
            // GAS, memory, CODECOPY, return: fixed ordinary cost 35 gas.
            let code = Bytes::from_static(&[
                0x5a, 0x60, 0, 0x52, 0x60, 1, 0x60, 0, 0x60, 32, 0x39, 0x60, 64, 0x60, 0, 0xf3,
            ]);
            let provider = Provider {
                hash: keccak256(&code),
                code,
                reads: Rc::default(),
                full_reads: Rc::default(),
            };
            let reads = provider.reads.clone();
            let mut execution = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
                ExecutionConfig::for_spec_and_version(spec, version),
                spec,
                BlockEnvExt::default(),
                TxRegistry::new(),
                Db::new(provider),
                Precompiles::base(spec),
            );
            let result = execution
                .system_call(
                    SystemTx::new(Address::repeat_byte(0x44), Bytes::new()).with_gas_limit(100_000),
                )
                .unwrap()
                .discard();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.total_gas_spent, 35);
            assert_eq!(result.output.len(), 64);
            assert_eq!(U256::from_be_slice(&result.output[..32]), U256::from(99_998));
            assert_eq!(result.output[32], 0x5a);
            assert!(result.output[33..].iter().all(|byte| *byte == 0));
            assert_eq!(*reads.borrow(), [0]);
        }
        let enabled = Version::new(spec).with_tip1143(true);
        assert_eq!(enabled.max_code_size, 981640);
        assert_eq!(enabled.max_initcode_size, 1966080);
    }
}

#[test]
fn tip1143_t22_copy_warm_last_and_all_warm_budget_rows() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    let length = 2 * CHUNK + 1;
    for all_warm in [false, true] {
        let mut payload = Vec::new();
        push3(&mut payload, 3 * CHUNK);
        payload.push(0x56);
        payload.resize(3 * CHUNK, 0);
        payload.push(0x5b);
        let target_hash = keccak256(&payload);
        let mut code = Vec::new();
        push3(&mut code, length);
        push3(&mut code, CHUNK);
        code.extend([0x60, 0]);
        push_address(&mut code, target);
        code.extend([0x3c, 0]);
        let caller_hash = keccak256(&code);
        let words = (length as u64).div_ceil(32);
        let ordinary = 12 + 100 + 6 * words + words * words / 512;
        let total = ordinary + if all_warm { 4 * WARM } else { 3 * COLD + WARM };
        for budget in [total - 1, total, total + 1] {
            let mut world = World::default();
            world.accounts.insert(caller, Bytes::copy_from_slice(&code));
            world.accounts.insert(target, Bytes::copy_from_slice(&payload));
            let reads = world.reads.clone();
            let mut execution = world_evm(world, true);
            if all_warm {
                let warmup =
                    execution.execute_system_call(SystemTx::new(caller, Bytes::new())).unwrap();
                assert_eq!(warmup.stop, InstrStop::Stop);
                assert_eq!(
                    *reads.borrow(),
                    [(caller_hash, 0), (target_hash, 1), (target_hash, 2), (target_hash, 3)]
                );
            } else {
                let warmup =
                    execution.execute_system_call(SystemTx::new(target, Bytes::new())).unwrap();
                assert_eq!(warmup.stop, InstrStop::Stop);
                assert_eq!(warmup.total_gas_spent, 2 * COLD + 12);
                assert_eq!(*reads.borrow(), [(target_hash, 0), (target_hash, 3)]);
            }
            let before_reads = reads.borrow().clone();
            let before = execution.state_mut().code_chunk_stats();
            let result = execution
                .execute_system_call(SystemTx::new(caller, Bytes::new()).with_gas_limit(budget))
                .unwrap();
            let after = execution.state_mut().code_chunk_stats();
            let mut expected = before_reads;
            if !all_warm {
                expected.push((caller_hash, 0));
            }
            if budget < total {
                assert_eq!(result.stop, InstrStop::OutOfGas);
                assert_eq!(
                    after.logical_cold_accesses - before.logical_cold_accesses,
                    u64::from(!all_warm)
                );
                assert_eq!(
                    after.logical_warm_accesses - before.logical_warm_accesses,
                    u64::from(all_warm)
                );
            } else {
                assert_eq!(result.stop, InstrStop::Stop);
                assert_eq!(result.total_gas_spent, total);
                if !all_warm {
                    expected.extend([(target_hash, 1), (target_hash, 2)]);
                }
                assert_eq!(
                    after.logical_cold_accesses - before.logical_cold_accesses,
                    if all_warm { 0 } else { 3 }
                );
                assert_eq!(
                    after.logical_warm_accesses - before.logical_warm_accesses,
                    if all_warm { 4 } else { 1 }
                );
            }
            assert_eq!(*reads.borrow(), expected);
        }
    }
}

#[test]
fn tip1143_t22_legacy_size_budget_brackets_preserve_preexisting_warmth() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    let payload = Bytes::from(vec![0; 17]);
    let target_hash = keccak256(&payload);
    for warm in [false, true] {
        // PUSH20 and EXTCODESIZE; STOP costs zero.
        let ordinary = 3 + if warm { 100 } else { 2600 };
        let total = COLD + ordinary + if warm { WARM } else { COLD };
        for budget in [total - 1, total, total + 1] {
            let mut code = Vec::new();
            push_address(&mut code, target);
            code.extend([0x3b, 0]);
            let caller_hash = keccak256(&code);
            let mut world = World::default();
            world.accounts.insert(caller, code.into());
            world.accounts.insert(target, payload.clone());
            let reads = world.reads.clone();
            let mut execution = world_evm(world, true);
            if warm {
                let result =
                    execution.execute_system_call(SystemTx::new(target, Bytes::new())).unwrap();
                assert_eq!(result.total_gas_spent, COLD);
            }
            let before = execution.state_mut().code_chunk_stats();
            let result = execution
                .execute_system_call(SystemTx::new(caller, Bytes::new()).with_gas_limit(budget))
                .unwrap();
            let after = execution.state_mut().code_chunk_stats();
            let mut expected = if warm { vec![(target_hash, 0)] } else { vec![] };
            expected.push((caller_hash, 0));
            if budget >= total {
                assert_eq!(result.stop, InstrStop::Stop);
                assert_eq!(result.total_gas_spent, total);
                if !warm {
                    expected.push((target_hash, 0));
                }
                assert_eq!(
                    after.logical_cold_accesses - before.logical_cold_accesses,
                    if warm { 1 } else { 2 }
                );
                assert_eq!(
                    after.logical_warm_accesses - before.logical_warm_accesses,
                    u64::from(warm)
                );
            } else {
                assert_eq!(result.stop, InstrStop::OutOfGas);
                assert_eq!(after.logical_cold_accesses - before.logical_cold_accesses, 1);
                assert_eq!(after.logical_warm_accesses - before.logical_warm_accesses, 0);
            }
            assert_eq!(*reads.borrow(), expected);
        }
    }
}

#[test]
fn tip1143_t18_t22_full_forty_chunk_codecopy_exact_budget_and_original_bytes() {
    for length in [39 * CHUNK + 1, 40 * CHUNK] {
        let mut code = Vec::new();
        push3(&mut code, length);
        code.extend([0x60, 0, 0x60, 0, 0x39]);
        push3(&mut code, length);
        code.extend([0x60, 0, 0xf3]);
        code.resize(length, 0);
        for index in 1..40 {
            // Distinguish payloads without crossing an instruction boundary.
            // A one-byte last payload is a complete JUMPDEST.
            code[index * CHUNK] = 0x5b;
            if index * CHUNK + 2 < length {
                code[index * CHUNK + 1] = 0x60;
                code[index * CHUNK + 2] = index as u8;
            }
        }
        let words = (length as u64).div_ceil(32);
        let before_return = COLD + WARM + 39 * COLD + 12 + 6 * words + words * words / 512;
        // The operation itself needs before_return; stop immediately afterwards
        // for its exact gas bracket, then separately verify the complete output.
        let mut bracket = code.clone();
        bracket[9] = 0;
        for budget in [before_return - 1, before_return, before_return + 1] {
            let result = execute(bracket.clone(), true, budget);
            if budget < before_return {
                assert_eq!(result.0, InstrStop::OutOfGas);
                assert_eq!(result.3, [0], "length={length}");
            } else {
                assert_eq!(result.0, InstrStop::Stop);
                assert_eq!(result.2, before_return);
                assert_eq!(result.3, (0..40).collect::<Vec<_>>());
            }
            assert_eq!(result.4, 0);
        }
        let result = execute(code.clone(), true, before_return + 6);
        assert_eq!(result.0, InstrStop::Return);
        assert_eq!(result.1.as_ref(), code);
        assert_eq!(result.2, before_return + 6);
        assert_eq!(result.3, (0..40).collect::<Vec<_>>());
        assert_eq!(result.4, 0);
    }
}

#[test]
fn tip1143_t15_t16_taken_conditional_jumps_reject_wide_aliases_and_push_data() {
    for conditional in [false, true] {
        for target in [U256::MAX, (U256::from(1) << 128) + U256::from(CHUNK), U256::from(CHUNK + 5)]
        {
            let mut code = Vec::new();
            if conditional {
                code.extend([0x60, 1]);
            }
            push_word(&mut code, target);
            code.push(if conditional { 0x57 } else { 0x56 });
            code.resize(CHUNK, 0);
            code.extend([0x5b, 0x60, 0x5b, 0x50, 0]);
            let result = execute(code, true, 100_000);
            assert_eq!(result.0, InstrStop::InvalidJump);
            assert_eq!(result.3, [0], "conditional={conditional}, target={target}");
        }
        for (local, valid) in [(0, true), (2, false), (3, false)] {
            let mut code = Vec::new();
            if conditional {
                code.extend([0x60, 1]);
            }
            push3(&mut code, CHUNK + local);
            code.push(if conditional { 0x57 } else { 0x56 });
            code.resize(CHUNK, 0);
            code.extend([0x5b, 0x60, 0x5b, 0x50, 0]);
            let result = execute(code, true, 100_000);
            assert_eq!(result.0, if valid { InstrStop::Stop } else { InstrStop::InvalidJump });
            assert_eq!(result.3, [0, 1]);
            if valid {
                // PUSH target/JUMP, JUMPDEST, PUSH immediate and POP.
                let ordinary = if conditional { 3 + 3 + 10 } else { 3 + 8 } + 1 + 3 + 2;
                assert_eq!(result.2, 2 * COLD + ordinary);
            }
        }
    }
}

#[test]
fn tip1143_t15_same_chunk_loop_has_no_repeated_chunk_tariff() {
    for iterations in [1_u8, 2, 17, 255] {
        let mut code = vec![0x60, iterations];
        let target = code.len();
        // Decrement the counter, preserve it across the conditional jump, and
        // return a marker only after every iteration has executed.
        code.extend([0x5b, 0x60, 1, 0x90, 0x03, 0x80]);
        push3(&mut code, target);
        code.extend([0x57, 0x50, 0x60, 0x42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        code.resize(2 * CHUNK, 0);
        let ordinary = 3 + 26 * u64::from(iterations) + 20;
        let result = execute(code, true, COLD + ordinary);
        assert_eq!(result.0, InstrStop::Return, "iterations={iterations}");
        assert_eq!(U256::from_be_slice(&result.1), U256::from(0x42));
        assert_eq!(result.2, COLD + ordinary);
        assert_eq!(result.3, [0]);
        assert_eq!(result.4, 0);
    }
}

#[test]
fn tip1143_t22_t23_call_entry_exact_budget_reserves_before_child_fetch() {
    let parent = Address::repeat_byte(0x44);
    let child = Address::repeat_byte(0x77);
    for multi in [false, true] {
        for warm in [false, true] {
            for extra in [-1_i64, 0, 1] {
                // A zero-gas request can execute STOP, but cannot exempt entry
                // from its tariff. Seven PUSHes and account access are ordinary
                // CALL costs; zero-sized input/output requires no expansion.
                let mut caller = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0];
                push_address(&mut caller, child);
                caller.extend([0x60, 0, 0xf1, 0x00]);
                let callee = vec![0; if multi { CHUNK + 1 } else { 1 }];
                let caller_hash = keccak256(&caller);
                let callee_hash = keccak256(&callee);
                let mut world = World::default();
                world.accounts.insert(parent, caller.into());
                world.accounts.insert(child, callee.into());
                let reads = world.reads.clone();
                let mut execution = world_evm(world, true);
                if warm {
                    let control =
                        execution.execute_system_call(SystemTx::new(parent, Bytes::new())).unwrap();
                    assert_eq!(control.stop, InstrStop::Stop);
                    assert_eq!(*reads.borrow(), [(caller_hash, 0), (callee_hash, 0)]);
                    reads.borrow_mut().clear();
                }
                let tariff = if warm { WARM } else { COLD };
                let ordinary_account = if warm { 100 } else { 2600 };
                let exact = 2 * tariff + 21 + ordinary_account;
                let budget = (exact as i64 + extra) as u64;
                let result = execution
                    .execute_system_call(SystemTx::new(parent, Bytes::new()).with_gas_limit(budget))
                    .unwrap();
                assert_eq!(
                    result.stop,
                    if extra < 0 { InstrStop::OutOfGas } else { InstrStop::Stop },
                    "multi={multi}, warm={warm}, extra={extra}"
                );
                assert_eq!(result.total_gas_spent, if extra < 0 { budget } else { exact });
                let expected = if warm {
                    vec![]
                } else if extra < 0 {
                    vec![(caller_hash, 0)]
                } else {
                    vec![(caller_hash, 0), (callee_hash, 0)]
                };
                assert_eq!(*reads.borrow(), expected);
            }
        }
    }
}

/// A resident immutable cache cannot make an unaffordable logical request free.
#[test]
fn tip1143_t20_t22_copy_budget_matrix_with_retained_physical_cache() {
    let caller = Address::repeat_byte(0x44);
    let target = Address::repeat_byte(0x77);
    for multi in [false, true] {
        let length = if multi { 2 * CHUNK + 1 } else { 4 };
        let source = if multi { 0 } else { 17 };
        let payload = Bytes::from(vec![0; if multi { length } else { 17 }]);
        let target_hash = keccak256(&payload);
        let count = if multi { 3 } else { 1 };
        let mut code = Vec::new();
        push3(&mut code, length);
        push3(&mut code, source);
        code.extend([0x60, 0]);
        push_address(&mut code, target);
        code.extend([0x3c, 0]);
        let caller_hash = keccak256(&code);
        for (cached, warm) in [(false, false), (true, false), (true, true)] {
            let tariff = if warm { WARM } else { COLD };
            let words = (length as u64).div_ceil(32);
            let total = (count + 1) * tariff
                + 12
                + if warm { 100 } else { 2600 }
                + 6 * words
                + words * words / 512;
            for budget in [total - 1, total, total + 1] {
                let mut world = World::default();
                world.accounts.insert(caller, Bytes::copy_from_slice(&code));
                world.accounts.insert(target, payload.clone());
                let reads = world.reads.clone();
                let mut execution = world_evm(world, true);
                if cached {
                    let features = execution.version().features;
                    let checkpoint = execution.state_mut().checkpoint();
                    let warmup =
                        execution.execute_system_call(SystemTx::new(caller, Bytes::new())).unwrap();
                    assert_eq!(warmup.stop, InstrStop::Stop);
                    assert_eq!(reads.borrow().len() as u64, count + 1);
                    if !warm {
                        execution.state_mut().rollback(checkpoint, features);
                    }
                }
                let before = execution.state_mut().code_chunk_stats();
                let before_reads = reads.borrow().clone();
                let result = execution
                    .execute_system_call(SystemTx::new(caller, Bytes::new()).with_gas_limit(budget))
                    .unwrap();
                let after = execution.state_mut().code_chunk_stats();
                let accesses = if budget < total { 1 } else { count + 1 };
                assert_eq!(
                    after.logical_cold_accesses - before.logical_cold_accesses,
                    if warm { 0 } else { accesses }
                );
                assert_eq!(
                    after.logical_warm_accesses - before.logical_warm_accesses,
                    if warm { accesses } else { 0 }
                );
                if budget < total {
                    assert_eq!(result.stop, InstrStop::OutOfGas);
                } else {
                    assert_eq!(result.stop, InstrStop::Stop);
                    assert_eq!(result.total_gas_spent, total);
                }
                if cached {
                    assert_eq!(*reads.borrow(), before_reads);
                } else {
                    let mut expected = vec![(caller_hash, 0)];
                    if budget >= total {
                        expected.extend((0..count as u32).map(|index| (target_hash, index)));
                    }
                    assert_eq!(*reads.borrow(), expected);
                }
            }
        }
    }
}

#[test]
fn tip1143_t32_resident_initcode_external_operations_bypass_runner_only_when_enabled() {
    let caller = Address::repeat_byte(0x55);
    let target = Address::repeat_byte(0x77);
    for enabled in [false, true] {
        for operation in [0, 1, 2] {
            let mut code = Vec::new();
            let (payload, output, ordinary) = match operation {
                0 => {
                    push_address(&mut code, target);
                    code.extend([0x3b, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
                    let mut output = vec![0; 32];
                    output[31] = 1;
                    (vec![0x5b], output, 2618 + 6400)
                }
                1 => {
                    code.extend([0x60, 1, 0x60, 0, 0x60, 0]);
                    push_address(&mut code, target);
                    code.extend([0x3c, 0x60, 1, 0x60, 0, 0xf3]);
                    (vec![0x5b], vec![0x5b], 2624 + 200)
                }
                2 => {
                    code.extend([0x60, 32, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0]);
                    push_address(&mut code, target);
                    push3(&mut code, 100_000);
                    code.extend([0xf1, 0x60, 32, 0x60, 0, 0xf3]);
                    let mut output = vec![0; 32];
                    output[31] = 42;
                    (vec![0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3], output, 9048)
                }
                _ => unreachable!(),
            };
            let hash = keccak256(&payload);
            let mut world = World::default();
            world.accounts.insert(target, payload.into());
            let reads = world.reads.clone();
            let mut execution = world_evm(world, enabled);
            let calls = Arc::new(AtomicUsize::new(0));
            execution.set_interpreter_runner(RunnerSpy(calls.clone()));
            let mut gas = GasTracker::new(1_000_000);
            let frame = prepare_initial_frame(
                &mut execution,
                caller,
                0,
                TxKind::Create,
                &code.into(),
                U256::ZERO,
                &mut gas,
            )
            .unwrap();
            let result = execute_initial_frame(
                &mut execution,
                &TxEnvExt::default(),
                frame,
                &mut gas,
                1_000_000,
                0,
            )
            .unwrap();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(result.created_address, Some(caller.create(0)));
            assert_eq!(result.output.as_ref(), output);
            assert_eq!(gas.spent(), ordinary + if enabled { COLD } else { 0 });
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if enabled { 0 } else { 1 + usize::from(operation == 2) }
            );
            assert_eq!(*reads.borrow(), vec![(hash, 0)]);
            let account = execution.state_mut().account(&caller.create(0)).unwrap();
            assert_eq!(account.get().unwrap().code_hash, keccak256(&output));
            assert!(account.get().unwrap().code_metadata.is_none());
        }
    }
}

/// TIP1143-T13: nested dispatch, including a precompile with a runtime row.
#[test]
fn tip1143_t13_nested_empty_and_precompile_calls_skip_runtime_chunks() {
    let parent = Address::repeat_byte(0x66);
    let absent = Address::repeat_byte(0x41);
    let empty = Address::repeat_byte(0x42);
    let identity = Address::with_last_byte(4);
    for target in [absent, empty, identity] {
        let mut code = Vec::new();
        // CALL with empty input/output, zero value, then return its success flag.
        for _ in 0..5 {
            code.extend([0x60, 0]);
        }
        push_address(&mut code, target);
        push3(&mut code, 100_000);
        code.extend([0xf1, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
        let hash = keccak256(&code);
        let mut world = World::default();
        world.accounts.insert(parent, code.into());
        world.accounts.insert(empty, Bytes::new());
        world.accounts.insert(identity, Bytes::from_static(&[0xfe]));
        let reads = world.reads.clone();
        let mut execution = world_evm(world, true);
        execution.state_mut().account(&target).unwrap().warm();
        for tariff in [COLD, WARM] {
            let result =
                execution.execute_system_call(SystemTx::new(parent, Bytes::new())).unwrap();
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(Word::from_be_slice(&result.output), Word::from(1));
            // Seven PUSHes, warm CALL, PUSH/MSTORE plus memory expansion, two
            // return PUSHes; identity charges 15 for the empty input.
            assert_eq!(
                result.total_gas_spent,
                tariff + 21 + 100 + 3 + 6 + 6 + if target == identity { 15 } else { 0 }
            );
            assert_eq!(*reads.borrow(), [(hash, 0)]);
            assert!(execution.state_mut().account(&target).unwrap().code_chunks().is_empty());
            assert!(result.logs.is_empty());
        }
    }
}

/// TIP1143-T22: a cached and logically warm remote jump still reserves its tariff.
#[test]
fn tip1143_t22_warm_remote_jump_exact_operation_budget() {
    let owner = Address::repeat_byte(0x44);
    let mut code = Vec::new();
    push3(&mut code, CHUNK);
    code.push(0x56);
    code.resize(CHUNK, 0);
    code.extend([0x5b, 0]);
    let hash = keccak256(&code);
    // At operation budget, JUMP has paid its 8 gas but JUMPDEST still needs 1.
    let operation_budget = 2 * WARM + 3 + 8;
    for budget in [operation_budget - 1, operation_budget, operation_budget + 1] {
        let mut world = World::default();
        world.accounts.insert(owner, Bytes::copy_from_slice(&code));
        let reads = world.reads.clone();
        let mut execution = world_evm(world, true);
        let first = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(first.total_gas_spent, 2 * COLD + 12);
        let before = execution.state_mut().code_chunk_stats();
        let result = execution
            .execute_system_call(SystemTx::new(owner, Bytes::new()).with_gas_limit(budget))
            .unwrap();
        assert_eq!(
            result.stop,
            if budget > operation_budget { InstrStop::Stop } else { InstrStop::OutOfGas }
        );
        assert_eq!(result.total_gas_spent, budget);
        let after = execution.state_mut().code_chunk_stats();
        assert_eq!(
            after.logical_warm_accesses - before.logical_warm_accesses,
            if budget < operation_budget { 1 } else { 2 }
        );
        assert_eq!(after.logical_cold_accesses, before.logical_cold_accesses);
        assert_eq!(after.analyzed_chunks, before.analyzed_chunks);
        assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1)]);
        assert!(
            execution
                .state_mut()
                .account(&owner)
                .unwrap()
                .code_chunks()
                .values()
                .all(|chunk| chunk.is_warm)
        );
    }
}

/// TIP1143-T38: payload equality must never merge account-index warmth.
#[test]
fn tip1143_t38_identical_remote_payloads_pay_distinct_logical_tariffs() {
    let owner = Address::repeat_byte(0x44);
    let mut code = vec![0x60, 2];
    push3(&mut code, 2 * CHUNK - 1);
    code.extend([0x60, 0, 0x39, 0]);
    code.resize(3 * CHUNK, 0);
    assert_eq!(&code[CHUNK..2 * CHUNK], &code[2 * CHUNK..]);
    let hash = keccak256(&code);
    let mut world = World::default();
    world.accounts.insert(owner, code.into());
    let reads = world.reads.clone();
    let mut execution = world_evm(world, true);
    for tariff in [COLD, WARM] {
        let result = execution.execute_system_call(SystemTx::new(owner, Bytes::new())).unwrap();
        assert_eq!(result.stop, InstrStop::Stop);
        // Three PUSHes, COPY base/one word, and expansion by one memory word.
        assert_eq!(result.total_gas_spent, 3 * tariff + 18);
        assert_eq!(*reads.borrow(), [(hash, 0), (hash, 1), (hash, 2)]);
        let account = execution.state_mut().account(&owner).unwrap();
        assert_eq!(account.code_chunks().len(), 3);
        for index in 0..3 {
            assert!(account.code_chunks().get(&index).unwrap().is_warm);
        }
    }
    let stats = execution.state_mut().code_chunk_stats();
    assert_eq!(stats.logical_cold_accesses, 3);
    assert_eq!(stats.logical_warm_accesses, 3);
}
