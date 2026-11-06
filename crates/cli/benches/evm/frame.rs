//! Warm ordinary and frame execution across interpreter, inspection, JIT and AOT backends.

use alloy_consensus::{TxEip8141, TxLegacy, transaction::Recovered};
use alloy_eips::eip8141::{
    Frame, FrameLimits, FrameMode, FrameSignature, FrameStatus, NONCE_MANAGER, SignatureScheme,
    nonce_keys_hash,
};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use criterion::{BenchmarkGroup, Criterion, black_box, measurement::WallTime};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResult,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{RecoveredTxEnvelope, TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB, inspector::NoopInspector},
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};
use secp256k1::{Message, PublicKey, SecretKey, ecdsa::RecoverableSignature};
use std::{env, time::Duration};

#[cfg(feature = "jit")]
use evm2_jit_runtime::{
    evm2_evm::JitInterpreterRunner,
    runtime::{ArtifactStore, JitBackend, RuntimeArtifactStore, RuntimeConfig, RuntimeTuning},
};
#[cfg(feature = "jit")]
use std::sync::Arc;

const CALLER: Address = Address::with_last_byte(0xa0);
const BODY: Address = Address::with_last_byte(0xb0);
const ASSERTION: Address = Address::with_last_byte(0xc0);
const CHILD: Address = Address::with_last_byte(0xd0);

struct Case {
    name: String,
    spec: SpecId,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
    output: Option<U256>,
    check: Check,
}

#[derive(Clone, Copy)]
enum Check {
    Basic,
    Atomic { rollback: bool },
    NestedLog,
    FirstUse,
    Signed(Address),
    FrameLimit,
}

pub(crate) fn bench(c: &mut Criterion) {
    let shard = env::var("EVM2_BENCH_SHARD").ok();
    if shard.as_deref() == Some("jit-mainnet") || env::var_os("EVM2_BENCH_REVM").is_some() {
        return;
    }
    let mut cases = Vec::new();
    for spec in [SpecId::CANCUN, SpecId::AMSTERDAM, SpecId::BOGOTA] {
        for slots in [0, 1] {
            cases.push(ordinary(spec, slots));
        }
    }
    for slots in [0, 8, 64] {
        cases.push(frames(slots));
    }
    let mut control = frame_workload(64, 1, 1, false);
    control.name = "assertion_no_trace_slots_64".into();
    cases.push(control);
    for trace in [false, true] {
        cases.push(frame_workload(8, 8, 8, trace));
    }
    for reads in [1, 64] {
        cases.push(keyed(reads));
    }
    cases.extend([signed(), atomic(false), atomic(true), nested_log(), first_use(), frame_limit()]);
    cases.extend([payment_trace(0), payment_trace(64), tracing_workload()]);
    for slots in 0..=1 {
        let mut before = cases[2 + slots].evm();
        let mut after = cases[4 + slots].evm();
        assert_eq!(
            before.call_tx(&cases[2 + slots].tx).unwrap(),
            after.call_tx(&cases[4 + slots].tx).unwrap()
        );
    }
    let mut group = c.benchmark_group("frame");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(10);
    if shard.as_deref() != Some("jit") {
        for case in &cases {
            let mut evm = case.evm();
            bench_case(&mut group, case, &mut evm, &case.name);
            #[cfg(evm2_bench_allocations)]
            allocation::measure(case);
        }
        let case = &cases[8];
        let mut evm = case.evm();
        evm.set_inspector(NoopInspector::default());
        assert_eq!(evm.call_tx(&case.tx).unwrap(), case.evm().call_tx(&case.tx).unwrap());
        bench_case(&mut group, case, &mut evm, &format!("{}/inspected", case.name));
        let traced =
            cases.iter().find(|case| case.name == "tracing_frames_64_nested_calls_128").unwrap();
        bench_tracing(&mut group, traced);
        let signed = cases.iter().find(|case| case.name == "default_eoa_secp256k1").unwrap();
        bench_prefix(&mut group, signed);
    }
    #[cfg(feature = "jit")]
    if shard.as_deref() != Some("interpreter") {
        let keyed =
            cases.iter().find(|case| case.name == "keyed_nonce_hash_keys_8_reads_64").unwrap();
        bench_compiled(&mut group, [&cases[8], keyed]);
    }
    group.finish();
}

impl Case {
    fn evm(&self) -> Evm<'static, BaseEvmTypes> {
        Evm::new(
            self.spec,
            BlockEnvExt::default(),
            ethereum_tx_registry(self.spec),
            self.db.clone(),
            Precompiles::base(self.spec),
        )
    }

    fn check_result(&self, result: &TxResult) {
        assert!(result.status, "{}", self.name);
        if let Some(expected) = self.output {
            assert_eq!(
                result.frame.as_ref().unwrap().outputs.last().unwrap().as_ref(),
                expected.to_be_bytes::<32>(),
                "{}",
                self.name,
            );
        }
        match self.check {
            Check::Basic => {}
            Check::Atomic { rollback } => {
                let frame = result.frame.as_ref().unwrap();
                let expected = if rollback {
                    vec![
                        FrameStatus::Success,
                        FrameStatus::Success,
                        FrameStatus::Failure,
                        FrameStatus::SkippedAtomicBatch,
                        FrameStatus::Success,
                    ]
                } else {
                    vec![FrameStatus::Success; 5]
                };
                assert_eq!(
                    frame.receipts.iter().map(|receipt| receipt.status).collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(frame.rolled_back, [false, rollback, false, false, false]);
            }
            Check::NestedLog => {
                assert_eq!(result.logs.len(), 1);
                assert_eq!(result.logs[0].address, CHILD);
                assert!(result.logs[0].data.topics().is_empty());
                assert_eq!(result.logs[0].data.data.as_ref(), [0xab]);
                assert_eq!(result.frame.as_ref().unwrap().receipts[1].logs, result.logs);
            }
            Check::FirstUse => {
                assert!(result.state_gas_spent > 0);
                assert!(result.frame.as_ref().unwrap().receipts[1].gas_used.state > 0);
            }
            Check::Signed(sender) => {
                let frame = result.frame.as_ref().unwrap();
                assert_eq!(frame.payer, sender);
                assert_eq!(frame.receipts[0].status, FrameStatus::Success);
            }
            Check::FrameLimit => {
                let frame = result.frame.as_ref().unwrap();
                assert_eq!(frame.receipts.len(), 64);
                assert!(
                    frame.receipts.iter().all(|receipt| receipt.status == FrameStatus::Success)
                );
            }
        }
    }
}

fn database(slots: u8, frame: bool, accounts: u8) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    let caller_code =
        if frame { Bytes::from_static(&[0x60, 3, 0x5f, 0x5f, 0xaa]) } else { Bytes::new() };
    db.insert_account_info(
        &CALLER,
        AccountInfo::default()
            .with_balance(U256::from(1_000_000_000_u64))
            .with_code(Bytecode::new_legacy(caller_code)),
    );
    for account in 0..accounts {
        let address = Address::with_last_byte(0xb0 + account);
        let mut code = Vec::new();
        for slot in 0..slots {
            code.extend_from_slice(&[0x60, 2, 0x60, slot, 0x55]);
            db.insert_account_storage(&address, &U256::from(slot), &U256::ONE);
        }
        code.push(0x00);
        db.insert_account_info(
            &address,
            AccountInfo::default()
                .with_balance(U256::ONE)
                .with_code(Bytecode::new_legacy(code.into())),
        );
    }
    db.insert_account_info(
        &ASSERTION,
        AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
            0x5f, 0x60, 1, 0xb6, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3,
        ]))),
    );
    db
}

fn ordinary(spec: SpecId, slots: u8) -> Case {
    Case {
        name: format!("ordinary_{spec:?}_slots_{slots}"),
        spec,
        db: database(slots, false, 1),
        tx: Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(BODY),
                gas_limit: 1_000_000,
                value: U256::ONE,
                ..Default::default()
            }),
            CALLER,
        ),
        output: None,
        check: Check::Basic,
    }
}

fn frames(slots: u8) -> Case {
    let mut case = frame_workload(slots, 1, 1, true);
    case.name = format!("assertion_diff_slots_{slots}");
    case
}

fn frame_workload(slots: u8, accounts: u8, assertions: usize, trace: bool) -> Case {
    let mut db = database(slots, true, accounts);
    let changed = slots * accounts;
    if !trace {
        // Return the same value with the same instruction count, without requesting the diff.
        db.insert_account_info(
            &ASSERTION,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from(vec![
                0x60, changed, 0x5f, 0x50, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3,
            ]))),
        );
    }
    Case {
        name: format!(
            "assertion_{}_accounts_{accounts}_slots_{slots}_assertions_{assertions}",
            if trace { "diff" } else { "no_trace" }
        ),
        spec: SpecId::BOGOTA,
        db,
        tx: Recovered::new_unchecked(
            TxEnvelope::Eip8141(TxEip8141 {
                chain_id: 1,
                sender: CALLER,
                nonce_keys: Some(vec![]),
                frames: core::iter::once((FrameMode::Verify, CALLER, 3))
                    .chain(
                        (0..accounts).map(|index| {
                            (FrameMode::Sender, Address::with_last_byte(0xb0 + index), 0)
                        }),
                    )
                    .chain(core::iter::repeat_n((FrameMode::Assert, ASSERTION, 0), assertions))
                    .map(|(mode, target, flags)| Frame {
                        mode,
                        target: target.into(),
                        flags,
                        limits: FrameLimits {
                            execution: if accounts == 1 { 1_000_000 } else { 100_000 },
                            state: 100_000,
                        },
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            Address::ZERO,
        ),
        output: Some(U256::from(changed)),
        check: Check::Basic,
    }
}

fn keyed(reads: usize) -> Case {
    let keys = (1_u64..=8).map(U256::from).collect::<Vec<_>>();
    let mut db = database(0, true, 1);
    db.insert_account_info(&NONCE_MANAGER, AccountInfo::default().with_nonce(1));
    let mut code = Vec::new();
    for _ in 1..reads {
        code.extend_from_slice(&[0x60, 15, 0xb0, 0x50]);
    }
    code.extend_from_slice(&[0x60, 15, 0xb0, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
    db.insert_account_info(
        &BODY,
        AccountInfo::default().with_code(Bytecode::new_legacy(code.into())),
    );
    Case {
        name: format!("keyed_nonce_hash_keys_8_reads_{reads}"),
        spec: SpecId::BOGOTA,
        db,
        output: Some(U256::from_be_bytes(nonce_keys_hash(&keys).0)),
        check: Check::Basic,
        tx: Recovered::new_unchecked(
            TxEnvelope::Eip8141(TxEip8141 {
                chain_id: 1,
                sender: CALLER,
                nonce_keys: Some(keys),
                frames: [(FrameMode::Verify, CALLER, 3), (FrameMode::Sender, BODY, 0)]
                    .into_iter()
                    .map(|(mode, target, flags)| Frame {
                        mode,
                        target: target.into(),
                        flags,
                        // Eight fresh keyed nonces cost 8 * 97_920 state gas at Bogota.
                        limits: FrameLimits { execution: 1_000_000, state: 1_000_000 },
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            Address::ZERO,
        ),
    }
}

fn bench_case(
    group: &mut BenchmarkGroup<'_, WallTime>,
    case: &Case,
    evm: &mut Evm<'_, BaseEvmTypes>,
    name: &str,
) {
    let result = evm.call_tx(&case.tx).unwrap();
    case.check_result(&result);
    group.bench_function(name, |b| {
        b.iter(|| black_box(evm.call_tx(black_box(&case.tx)).unwrap()));
    });
}

#[cfg(feature = "jit")]
fn bench_compiled(group: &mut BenchmarkGroup<'_, WallTime>, cases: [&Case; 2]) {
    for aot in [false, true] {
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            aot,
            store: aot
                .then(|| Arc::new(RuntimeArtifactStore::new().unwrap()) as Arc<dyn ArtifactStore>),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        })
        .unwrap();
        for case in cases {
            let name = format!("{}/{}", case.name, if aot { "aot" } else { "jit" });
            let mut evm = case.evm();
            let expected = evm.call_tx(&case.tx).unwrap();
            evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
            // Compile before timing, then require every top-level frame to hit compiled code.
            assert_eq!(evm.call_tx(&case.tx).unwrap(), expected, "{name}");
            let before = backend.stats();
            assert_eq!(evm.call_tx(&case.tx).unwrap(), expected, "{name}");
            let warm = backend.stats();
            let roots = case.tx.inner().as_eip8141().unwrap().frames.len() as u64;
            assert_eq!(warm.lookup_hits - before.lookup_hits, roots, "{name}");
            assert_eq!(warm.lookup_misses, before.lookup_misses, "{name}");
            assert_eq!(warm.compilations_failed, 0, "{name}");
            bench_case(group, case, &mut evm, &name);
            let after = backend.stats();
            assert_eq!(after.lookup_misses, warm.lookup_misses, "{name}");
            assert_eq!(after.compilations_dispatched, warm.compilations_dispatched, "{name}");
            assert_eq!(after.compilations_failed, 0, "{name}");
        }
    }
}

fn signed() -> Case {
    let key = SecretKey::from_secret_bytes([0x11; 32]).unwrap();
    let public_key = PublicKey::from_secret_key(&key).serialize_uncompressed();
    let sender = Address::from_raw_public_key(&public_key[1..]);
    let mut case = frame_workload(8, 1, 1, true);
    case.name = "default_eoa_secp256k1".into();
    case.check = Check::Signed(sender);
    case.db.insert_account_info(
        &sender,
        AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
    );
    let mut tx = case.tx.inner().as_eip8141().unwrap().clone();
    tx.sender = sender;
    tx.frames[0].target = sender.into();
    tx.signatures.push(FrameSignature {
        scheme: SignatureScheme::Secp256k1,
        signature: vec![0; 65].into(),
        ..Default::default()
    });
    let signature = RecoverableSignature::sign_ecdsa_recoverable(
        Message::from_digest(tx.signature_hash().0),
        &key,
    );
    let (recovery_id, signature) = signature.serialize_compact();
    let mut bytes = vec![recovery_id.to_u8()];
    bytes.extend_from_slice(&signature);
    tx.signatures[0].signature = bytes.into();
    case.tx = Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO);
    case
}

fn atomic(rollback: bool) -> Case {
    let mut case = frame_workload(8, 1, 1, true);
    case.name = format!("atomic_{}_slots_8", if rollback { "rollback_warmth" } else { "success" });
    case.check = Check::Atomic { rollback };
    case.output = Some(U256::from(if rollback { 0_u8 } else { 8 }));
    case.db.insert_account_info(
        &CHILD,
        AccountInfo::default().with_code(Bytecode::new_legacy(if rollback {
            Bytes::from_static(&[0x5f, 0x5f, 0xfd])
        } else {
            Bytes::from_static(&[0x00])
        })),
    );
    let mut tx = case.tx.inner().as_eip8141().unwrap().clone();
    let assertion = tx.frames.pop().unwrap();
    tx.frames[1].flags = 4;
    tx.frames.push(Frame {
        mode: FrameMode::Sender,
        target: CHILD.into(),
        flags: 4,
        limits: tx.frames[1].limits,
        ..Default::default()
    });
    let mut terminal = tx.frames[1].clone();
    terminal.flags = 0;
    tx.frames.push(terminal);
    tx.frames.push(assertion);
    case.tx = Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO);
    case
}

fn nested_log() -> Case {
    let mut case = frame_workload(0, 1, 1, true);
    case.name = "nested_call_log_event_queries".into();
    case.check = Check::NestedLog;
    case.output = Some(U256::from(0xab));
    case.db.insert_account_info(
        &CHILD,
        AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
            0x60, 0xab, 0x5f, 0x53, 0x60, 1, 0x5f, 0xa0, 0x00,
        ]))),
    );
    // CALL forwards the remaining execution/state budgets to the event-emitting child.
    let mut body = vec![0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x73];
    body.extend_from_slice(CHILD.as_slice());
    body.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x00]);
    case.db.insert_account_info(
        &BODY,
        AccountInfo::default().with_code(Bytecode::new_legacy(body.into())),
    );
    case.db.insert_account_info(
        &ASSERTION,
        AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
            0x5f, 0x60, 0x11, 0xb6, 0x50, // TXTRACE event count
            0x5f, 0x60, 0x12, 0xb6, 0x50, // TXTRACE event address
            0x60, 1, 0x5f, 0x60, 31, 0x5f, 0xb7, // Copy the event byte to memory[31].
            0x60, 32, 0x5f, 0xf3,
        ]))),
    );
    case
}

fn first_use() -> Case {
    let mut case = frame_workload(16, 1, 1, true);
    case.name = "first_use_state_slots_16".into();
    case.check = Check::FirstUse;
    for slot in 0..16_u64 {
        case.db.insert_account_storage(&BODY, &U256::from(slot), &U256::ZERO);
    }
    let mut tx = case.tx.inner().as_eip8141().unwrap().clone();
    for frame in &mut tx.frames {
        // Sixteen fresh slots cost 16 * 97_920 state gas at Bogota.
        frame.limits.state = 2_000_000;
    }
    case.tx = Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO);
    case
}

fn frame_limit() -> Case {
    let mut case = frame_workload(8, 1, 62, true);
    case.name = "maximum_frames_64_assertions_62_slots_8".into();
    case.check = Check::FrameLimit;
    let mut tx = case.tx.inner().as_eip8141().unwrap().clone();
    for frame in &mut tx.frames {
        frame.limits.execution = 100_000;
    }
    case.tx = Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO);
    case
}

fn payment_trace(slots: u8) -> Case {
    let mut case = frame_workload(slots, 1, 1, true);
    case.name = format!("payment_only_trace_slots_{slots}_reads_128");
    case.output = Some(U256::from_be_slice(CALLER.as_slice()));
    let mut code = Vec::new();
    for _ in 0..64 {
        code.extend_from_slice(&[
            0x5f, 0x60, 0x19, 0xb6, 0x50, // TXTRACE gas pre-charge.
            0x5f, 0x60, 0x1a, 0xb6, 0x50, // TXTRACE payer address.
        ]);
    }
    code.pop();
    code.extend_from_slice(&[0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
    case.db.insert_account_info(
        &ASSERTION,
        AccountInfo::default().with_code(Bytecode::new_legacy(code.into())),
    );
    case
}

fn tracing_workload() -> Case {
    let mut case = frame_workload(0, 1, 62, false);
    case.name = "tracing_frames_64_nested_calls_128".into();
    case.check = Check::FrameLimit;
    case.db.insert_account_info(
        &CHILD,
        AccountInfo::default()
            .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 42, 0x50, 0x00]))),
    );
    // Put a large subtree before many small sibling frames. Excluded precompile nodes remain
    // in the arena, exercising their association with CALL steps during Parity rendering.
    let mut code = Vec::new();
    for _ in 0..64 {
        for target in [CHILD, Address::with_last_byte(4)] {
            code.extend_from_slice(&[0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x73]);
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[0x5a, 0xf1, 0x50]);
        }
    }
    code.push(0x00);
    case.db.insert_account_info(
        &BODY,
        AccountInfo::default().with_code(Bytecode::new_legacy(code.into())),
    );
    let mut tx = case.tx.inner().as_eip8141().unwrap().clone();
    for frame in &mut tx.frames {
        frame.limits.execution = 100_000;
    }
    case.tx = Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO);
    case
}

fn bench_tracing(group: &mut BenchmarkGroup<'_, WallTime>, case: &Case) {
    let mut evm = case.evm();
    let expected = evm.call_tx(&case.tx).unwrap();
    case.check_result(&expected);
    let config = TracingInspectorConfig::parity_vm_trace();
    evm.set_inspector(TracingInspector::new(config));
    assert_eq!(evm.call_tx(&case.tx).unwrap(), expected);
    let inspector = *evm.clear_inspector_as::<TracingInspector>().unwrap();
    assert_eq!(inspector.traces().nodes()[0].trace.gas_used, expected.tx_gas_used());
    let builder = inspector.into_parity_builder();
    let vm = builder.vm_trace();
    assert_eq!(vm.ops.len(), 64);
    assert_eq!(
        vm.ops.iter().map(|operation| operation.pc).collect::<Vec<_>>(),
        (0..64).collect::<Vec<_>>()
    );
    let body = vm.ops[1].sub.as_ref().unwrap();
    let children =
        body.ops.iter().filter_map(|operation| operation.sub.as_ref()).collect::<Vec<_>>();
    assert_eq!(children.len(), 128);
    for pair in children.as_chunks::<2>().0 {
        assert_eq!(pair[0].ops.len(), 3);
        assert!(pair[1].code.is_empty());
        assert!(pair[1].ops.is_empty());
    }
    assert!(vm.ops.iter().all(|operation| !operation.sub.as_ref().unwrap().ops.is_empty()));
    drop(vm);

    group.bench_function(format!("{}/tracing_record_drop", case.name), |b| {
        b.iter(|| {
            evm.set_inspector(TracingInspector::new(config));
            drop(black_box(evm.call_tx(black_box(&case.tx)).unwrap()));
            black_box(evm.clear_inspector_as::<TracingInspector>().unwrap());
        });
    });
    group.bench_function(format!("{}/parity_record_render", case.name), |b| {
        b.iter(|| {
            evm.set_inspector(TracingInspector::new(config));
            drop(black_box(evm.call_tx(black_box(&case.tx)).unwrap()));
            let inspector = *evm.clear_inspector_as::<TracingInspector>().unwrap();
            black_box(inspector.into_parity_builder().vm_trace());
        });
    });
    // The source arena is prepared once above; this case measures rendering and output drop.
    group.bench_function(format!("{}/parity_render_only", case.name), |b| {
        b.iter(|| black_box(black_box(&builder).vm_trace()));
    });
}

fn bench_prefix(group: &mut BenchmarkGroup<'_, WallTime>, case: &Case) {
    let mut evm = case.evm();
    let expected = evm.validate_frame_prefix(&case.tx, 1).unwrap();
    let sender = case.tx.inner().as_eip8141().unwrap().sender;
    assert_eq!(expected.payer, sender);
    assert_eq!(expected.prefix_end, 1);
    assert!(expected.sender_approved);
    assert!(expected.signature_gas > 0);
    assert!(expected.execution_gas > 0);
    assert_eq!(evm.validate_frame_prefix(&case.tx, 1).unwrap(), expected);
    // Prefix validation must leave the following full transaction's nonce, state, and result
    // intact.
    let full = evm.call_tx(&case.tx).unwrap();
    case.check_result(&full);
    assert_eq!(full, case.evm().call_tx(&case.tx).unwrap());
    group.bench_function(format!("{}/prefix_validation", case.name), |b| {
        b.iter(|| black_box(evm.validate_frame_prefix(black_box(&case.tx), 1).unwrap()));
    });
}

// Counting is opt-in so normal timing benchmarks keep the ordinary system allocator.
// Only the executing thread is observed; JIT workers and native allocator overhead are excluded.
// Build only this target with counting, then run the emitted executable with --test:
// cargo rustc --profile dev -p evm2-cli --bench evm -- --cfg evm2_bench_allocations
// EVM2_BENCH_SHARD=interpreter target/debug/deps/evm-<hash> --test
#[cfg(evm2_bench_allocations)]
mod allocation {
    use super::*;
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    #[derive(Clone, Copy, Default)]
    struct Counts {
        calls: usize,
        bytes: usize,
        live: i128,
        peak: i128,
    }

    thread_local! {
        static COUNTS: Cell<Option<Counts>> = const { Cell::new(None) };
    }

    struct Allocator;

    #[global_allocator]
    static ALLOCATOR: Allocator = Allocator;

    fn record(allocated: usize, freed: usize, allocation: bool) {
        let _ = COUNTS.try_with(|counter| {
            if let Some(mut counts) = counter.get() {
                counts.calls += usize::from(allocation);
                counts.bytes += allocated;
                counts.live += allocated as i128 - freed as i128;
                counts.peak = counts.peak.max(counts.live);
                counter.set(Some(counts));
            }
        });
    }

    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let result = unsafe { System.alloc(layout) };
            if !result.is_null() {
                record(layout.size(), 0, true);
            }
            result
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let result = unsafe { System.alloc_zeroed(layout) };
            if !result.is_null() {
                record(layout.size(), 0, true);
            }
            result
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let result = unsafe { System.realloc(pointer, layout, size) };
            if !result.is_null() {
                record(size, layout.size(), true);
            }
            result
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
            record(0, layout.size(), false);
        }
    }

    pub(super) fn measure(case: &Case) {
        const ITERATIONS: usize = 1_000;
        COUNTS.with(|counts| counts.set(Some(Counts::default())));
        let mut evm = case.evm();
        for _ in 0..10 {
            drop(black_box(evm.call_tx(&case.tx).unwrap()));
        }
        let warm = COUNTS.with(|counts| counts.get().unwrap());
        for _ in 0..ITERATIONS {
            drop(black_box(evm.call_tx(&case.tx).unwrap()));
        }
        let after = COUNTS.with(|counts| counts.get().unwrap());
        drop(evm);
        let counts = COUNTS.with(|counts| counts.replace(None).unwrap());
        println!(
            "allocations {}: {} calls/tx, {} requested bytes/tx; \
             thread requested bytes live={} peak={} after_drop={} warm_delta={}",
            case.name,
            (after.calls - warm.calls) as f64 / ITERATIONS as f64,
            (after.bytes - warm.bytes) as f64 / ITERATIONS as f64,
            after.live,
            counts.peak,
            counts.live,
            after.live - warm.live,
        );
    }
}
