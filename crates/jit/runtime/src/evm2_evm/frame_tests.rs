//! Frame transaction execution must use compiled code with the same gas and rollback semantics.

use super::*;
use crate::runtime::{ArtifactStore, RuntimeArtifactStore, RuntimeConfig, RuntimeTuning};
use alloy_consensus::{TxEip8141, TxLegacy, transaction::Recovered};
use alloy_eips::eip8141::{
    Frame, FrameLimits, FrameMode, FrameSignature, FrameStatus, SignatureScheme,
};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResultWithState,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
    interpreter::op,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn run(
    backend: Option<&JitBackend>,
    state: u64,
    revert: bool,
    event_opcode: u8,
) -> TxResultWithState {
    let sender = Address::with_last_byte(0xa0);
    let body = Address::with_last_byte(0xb0);
    let assertion = Address::with_last_byte(0xc0);
    let validation = [0x60, 3, 0x5f, 0x5f, 0xaa];
    let body_code = [
        0x5f, 0xb0, 0x50, // TXPARAM type
        0x60, 1, 0x60, 1, 0xb3, 0x50, // FRAMEPARAM execution budget
        0x60, 1, 0x5f, 0xb4, 0x50, // SIGPARAM scheme
        0x60, 1, 0x5f, 0xb1, 0x50, // FRAMEDATALOAD
        0x60, 1, 0x60, 32, 0x5f, 0x5f, 0xb2, // FRAMEDATACOPY
        0x5f, 0x60, 4, 0x5f, 0x60, 32, 0xb5, // SIGDATACOPY
        0x5f, 0x35, 0x5f, 0x55, // Ordinary SSTORE, with isolated state gas
        0x60, 36, 0x5f, 0xa0, 0x00, // Consensus event includes both copies
    ];
    let mut assertion_code = vec![
        0x5f,
        0x60,
        0x11,
        0xb6,
        0x50, // TXTRACE event count
        0x5f,
        0x60,
        1,
        0xb6,
        0x50, // TXTRACE changed storage slot count
        0x60,
        36,
        0x5f,
        0x5f,
        0x5f,
        event_opcode, // EVENTDATACOPY
    ];
    assertion_code.extend_from_slice(if revert {
        &[0x5f, 0x5f, 0xfd]
    } else {
        &[0x60, 36, 0x5f, 0xf3]
    });
    let mut database = InMemoryDB::default();
    for (address, code) in [
        (sender, validation.as_slice()),
        (body, body_code.as_slice()),
        (assertion, assertion_code.as_slice()),
    ] {
        database.insert_account_info(
            &address,
            AccountInfo::default()
                .with_balance(U256::from(1_000_000_000))
                .with_code(Bytecode::new_legacy(Bytes::copy_from_slice(code))),
        );
    }
    let frames = [
        (FrameMode::Verify, sender, 3),
        (FrameMode::Sender, body, 0),
        (FrameMode::Assert, assertion, 0),
    ]
    .into_iter()
    .map(|(mode, target, flags)| Frame {
        mode,
        target: target.into(),
        flags,
        limits: FrameLimits { execution: 100_000, state },
        data: U256::from(42).to_be_bytes::<32>().to_vec().into(),
        ..Default::default()
    })
    .collect();
    let tx = TxEip8141 {
        sender,
        chain_id: 1,
        frames,
        nonce_keys: Some(vec![]),
        signatures: vec![FrameSignature {
            scheme: SignatureScheme::Arbitrary,
            signature: Bytes::from_static(b"abcd"),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::BOGOTA,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::BOGOTA),
        database,
        Precompiles::base(SpecId::BOGOTA),
    );
    if let Some(backend) = backend {
        evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
    }
    evm.transact(&Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO))
        .unwrap()
        .detach()
}

#[test]
fn native_frames_match_interpreter_with_jit_aot_and_reloaded_artifacts() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    for aot in [false, true] {
        let store = Arc::new(RuntimeArtifactStore::new().unwrap()) as Arc<dyn ArtifactStore>;
        let config = RuntimeConfig {
            enabled: true,
            blocking: true,
            aot,
            single_error: false,
            store: aot.then_some(store),
            tuning: RuntimeTuning {
                jit_hot_threshold: 0,
                jit_worker_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut backend = JitBackend::new(config.clone()).unwrap();
        for reload in [false, true] {
            if reload && aot {
                backend = JitBackend::new(config.clone()).unwrap();
            }
            for state in [0, 300_000] {
                for revert in [false, true] {
                    let interpreted = run(None, state, revert, 0xb7);
                    let before = backend.stats().lookup_hits;
                    let compiled = run(Some(&backend), state, revert, 0xb7);
                    if state != 0 && !revert {
                        assert!(interpreted.result.status);
                        let mut expected = U256::from(42).to_be_bytes::<32>().to_vec();
                        expected.extend_from_slice(b"abcd");
                        assert_eq!(
                            interpreted.result.frame.as_ref().unwrap().outputs.last().unwrap(),
                            &Bytes::from(expected)
                        );
                    }
                    assert_eq!(
                        compiled.result, interpreted.result,
                        "aot={aot}, reload={reload}, state={state}, revert={revert}"
                    );
                    assert_eq!(compiled.pending_state, interpreted.pending_state);
                    let stats = backend.stats();
                    assert!(
                        stats.lookup_hits >= before + 3,
                        "every root must actually execute compiled code: aot={aot}, \
                         reload={reload}, state={state}, revert={revert}, \
                         previous_hits={before}, stats={stats:?}"
                    );
                    assert_eq!(stats.compilations_failed, 0);
                }
            }
            let interpreted = run(None, 300_000, false, 0xb8);
            let compiled = run(Some(&backend), 300_000, false, 0xb8);
            assert!(!interpreted.result.status);
            assert_eq!(
                interpreted.result.frame.as_ref().unwrap().receipts.last().unwrap().status,
                FrameStatus::Failure
            );
            assert_eq!(compiled.result, interpreted.result);
            assert_eq!(compiled.pending_state, interpreted.pending_state);
            assert_eq!(backend.stats().compilations_failed, 0);
        }
    }
}

#[test]
fn repeated_frames_reuse_compilations_and_release_cleared_programs() {
    for aot in [false, true] {
        let store = Arc::new(RuntimeArtifactStore::new().unwrap()) as Arc<dyn ArtifactStore>;
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            aot,
            single_error: false,
            store: aot.then(|| store.clone()),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        })
        .unwrap();
        // These two transactions contain four unique bytecodes: validation, body and the two
        // assertion endings. Fresh EVM instances and reverted state must not create new keys.
        for revert in [false, true] {
            let _ = run(Some(&backend), 300_000, revert, 0xb7);
        }
        let warm = backend.stats();
        assert_eq!(warm.resident_entries, 4);
        assert_eq!(warm.compilations_succeeded, 4);
        assert_eq!(warm.compilations_failed, 0);
        for repeat in 0..32 {
            let _ = run(
                Some(&backend),
                if repeat % 2 == 0 { 0 } else { 300_000 },
                repeat % 3 == 0,
                0xb7,
            );
            let stats = backend.stats();
            assert_eq!(stats.lookup_hits, warm.lookup_hits + (repeat + 1) * 3);
            assert_eq!(stats.resident_entries, warm.resident_entries);
            assert_eq!(stats.compilations_dispatched, warm.compilations_dispatched);
            assert_eq!(stats.compilations_failed, 0);
            assert_eq!(stats.pending_jobs, 0);
        }

        let program = backend
            .get_compiled(alloy_primitives::keccak256([0x60, 3, 0x5f, 0x5f, 0xaa]), SpecId::BOGOTA)
            .unwrap();
        let program_lifetime = Arc::downgrade(&program);
        drop(program);
        backend.clear_all();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if backend.stats().resident_entries == 0
                && program_lifetime.upgrade().is_none()
                && store.load_all().unwrap().is_empty()
            {
                break;
            }
            assert!(Instant::now() < deadline, "frame programs retained after clearing aot={aot}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn run_code_adoption(backend: Option<&JitBackend>, revert: bool) -> TxResultWithState {
    let sender = Address::with_last_byte(0xa0);
    let body = Address::with_last_byte(0xb0);
    let source = Address::with_last_byte(0xc0);
    let beneficiary = Address::with_last_byte(0xd0);
    let mut code = vec![op::PUSH20];
    code.extend_from_slice(source.as_slice());
    code.extend_from_slice(&[op::SETCODEFROM, op::POP]);
    if revert {
        code.extend_from_slice(&[op::PUSH0, op::PUSH0, op::REVERT]);
    } else {
        code.push(op::PUSH20);
        code.extend_from_slice(beneficiary.as_slice());
        code.push(op::SELFDESTRUCT);
    }
    let mut database = InMemoryDB::default();
    for (address, balance, code) in [
        (sender, 1_000_000_000, Bytecode::default()),
        (body, 7, Bytecode::new_legacy(code.into())),
        (source, 0, Bytecode::new_legacy(Bytes::from_static(&[op::STOP]))),
    ] {
        database.insert_account_info(
            &address,
            AccountInfo::default().with_balance(U256::from(balance)).with_code(code),
        );
    }
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::BOGOTA,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::BOGOTA),
        database,
        Precompiles::base(SpecId::BOGOTA),
    );
    if let Some(backend) = backend {
        evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
    }
    let tx = TxLegacy { gas_limit: 1_000_000, to: TxKind::Call(body), ..Default::default() };
    evm.transact(&Recovered::new_unchecked(TxEnvelope::Legacy(tx), sender)).unwrap().detach()
}

#[test]
fn code_adoption_and_sendall_match_interpreter_jit_aot_and_reloaded_artifacts() {
    for aot in [false, true] {
        let store = Arc::new(RuntimeArtifactStore::new().unwrap()) as Arc<dyn ArtifactStore>;
        let config = RuntimeConfig {
            enabled: true,
            blocking: true,
            aot,
            store: aot.then_some(store),
            tuning: RuntimeTuning {
                jit_hot_threshold: 0,
                jit_worker_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut backend = JitBackend::new(config.clone()).unwrap();
        for reload in [false, true] {
            if reload && aot {
                backend = JitBackend::new(config.clone()).unwrap();
            }
            for revert in [false, true] {
                let interpreted = run_code_adoption(None, revert);
                let before = backend.stats().lookup_hits;
                let compiled = run_code_adoption(Some(&backend), revert);
                assert_eq!(interpreted.result.status, !revert);
                assert_eq!(compiled.result, interpreted.result);
                assert_eq!(compiled.pending_state, interpreted.pending_state);
                assert!(backend.stats().lookup_hits > before);
                assert_eq!(backend.stats().compilations_failed, 0);
            }
        }
    }
}
