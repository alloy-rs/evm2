//! Native Bogota execution, journal, and settlement regressions.

mod common;

use alloy_consensus::{TxEip8141, TxLegacy, transaction::Recovered};
use alloy_eips::{
    Typed2718,
    eip8141::{Frame, FrameMode, FrameSignature, FrameStatus, SignatureMessage, SignatureScheme},
    eip8250::{NONCE_MANAGER, nonce_slot},
    eip8272::{RECENT_ROOT_ADDRESS, RECENT_ROOT_CODE, RECENT_ROOT_CODE_HASH},
};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use common::{
    APPROVE_BOTH, BODY, CountingRunner, FailingDatabase, SENDER, frame, install_recent_root,
    payload, recent_entry, recent_slot, recent_tuple, recovered, set_version, stored,
};
use evm2::{
    BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmFeatures, EvmTypesHost, FrameTransactionResult,
    Precompiles, SpecId, TxResultExt, Version,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{
        TxEnvelope,
        eip8141::{FrameGas, FrameHandler},
    },
    evm::{
        AccountInfo, Bal, BalError, BlockAccessIndex, Db, InMemoryDB,
        handler::DefaultTxHandlerHooks,
    },
    interpreter::{GasTracker, InstrStop},
    registry::{FrameTransactionError, HandlerError, TxRegistry},
    version::GasId,
};
use k256::ecdsa::SigningKey;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const ASSERTION: Address = Address::with_last_byte(0xc0);

fn evm(body: &[u8], assertion: &[u8]) -> Evm<'static, BaseEvmTypes> {
    let db = common::database_with_nonce_manager([
        (SENDER, APPROVE_BOTH),
        (BODY, body),
        (ASSERTION, assertion),
    ]);
    common::evm(db)
}

#[test]
fn state_gas_cannot_spill_into_execution() {
    let mut gas = GasTracker::new_isolated(1_000_000, 9);
    assert_eq!(gas.spend_state(10), Err(InstrStop::OutOfGas));
    assert_eq!(gas.remaining(), 1_000_000);
    assert_eq!(gas.reservoir(), 9);
    gas.spend_state(9).unwrap();
    assert_eq!(gas.remaining(), 1_000_000);
    assert_eq!(gas.reservoir(), 0);
}

#[test]
fn frame_floor_is_charged_in_addition_to_state() {
    let result = TxResultExt::<()> {
        total_gas_spent: 180_000,
        state_gas_spent: 100_000,
        floor_gas: 100_000,
        frame: Some(FrameTransactionResult::default()),
        ..Default::default()
    };
    assert_eq!(result.tx_gas_used(), 200_000);
    assert_eq!(result.execution_gas_spent(), 100_000);
    let mapped = result.map_ext(|()| 7);
    assert_eq!(mapped.tx_gas_used(), 200_000);
    assert_eq!(mapped.ext, 7);
    assert!(mapped.frame.is_some());
}

#[test]
fn frame_value_transfer_uses_the_active_gas_schedule() {
    let tx = payload(vec![
        Frame { value: U256::ONE, ..frame(FrameMode::Sender, BODY) },
        Frame { value: U256::ONE, ..frame(FrameMode::Sender, SENDER) },
        frame(FrameMode::Sender, BODY),
    ]);
    let mut version = Version::new(SpecId::BOGOTA);
    let baseline = FrameGas::calculate(&tx, &version).unwrap();
    version.gas_params.set(GasId::TxValueCost, 7_000);
    let repriced = FrameGas::calculate(&tx, &version).unwrap();
    assert_eq!(repriced.intrinsic, baseline.intrinsic + 1_000);
    assert_eq!(repriced.floor, baseline.floor + 1_000);
    assert_eq!(repriced.execution, baseline.execution + 1_000);
    assert_eq!(repriced.state, baseline.state);
}

#[rstest::rstest]
fn approval_executes_body_and_commits_explicit_sender_nonce(
    #[values(0, 7)] initial_call_depth: u16,
) {
    let mut evm = evm(&[0x60, 42, 0x5f, 0x55, 0x00], &[]);
    let mut version = Version::new(SpecId::BOGOTA);
    version.initial_call_depth = initial_call_depth;
    set_version(&mut evm, version);
    evm.tx_env_mut().origin = ASSERTION;
    evm.tx_env_mut().gas_price = U256::from(99);
    let tx =
        recovered(payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]));
    let result = evm.transact(&tx).unwrap().commit();
    let outcome = result.frame.unwrap();
    assert!(result.status);
    assert_eq!(outcome.payer, SENDER);
    assert_eq!(outcome.receipts.len(), 2);
    assert!(outcome.receipts.iter().all(|r| r.status == FrameStatus::Success));
    assert!(outcome.receipts[1].gas_used.state > 0);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::from(42));
    assert_eq!(evm.tx_env().origin, SENDER);
    assert_eq!(evm.tx_env().gas_price, U256::ZERO);
}

#[test]
fn assertion_failure_reverts_body_and_preserves_payment_prefix() {
    let mut evm = evm(&[0x60, 42, 0x5f, 0x55, 0x00], &[0x5f, 0x5f, 0xfd]);
    let tx = recovered(payload(vec![
        frame(FrameMode::Verify, SENDER),
        frame(FrameMode::Sender, BODY),
        frame(FrameMode::Assert, ASSERTION),
        frame(FrameMode::Assert, ASSERTION),
    ]));
    let result = evm.transact(&tx).unwrap().commit();
    assert!(!result.status);
    let outcome = result.frame.unwrap();
    assert_eq!(
        outcome.receipts.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![
            FrameStatus::Success,
            FrameStatus::Success,
            FrameStatus::Failure,
            FrameStatus::SkippedAtomicBatch
        ]
    );
    assert_eq!(outcome.receipts[1].gas_used.state, 0);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::ZERO);
}

#[test]
fn assertion_failure_preserves_sender_writes_before_payment_approval() {
    let payer = Address::with_last_byte(0xd0);
    let db = common::database_with_nonce_manager([
        (SENDER, &[0x60, 2, 0x5f, 0x5f, 0xaa][..]),
        (payer, &[0x60, 1, 0x5f, 0x5f, 0xaa]),
        (BODY, &[0x5f, 0x35, 0x5f, 0x55, 0x00]),
        (ASSERTION, &[0x5f, 0x5f, 0xfd]),
    ]);
    let mut evm = common::evm(db);
    let write = |value: u8| Frame {
        data: U256::from(value).to_be_bytes::<32>().to_vec().into(),
        ..frame(FrameMode::Sender, BODY)
    };
    let tx = recovered(payload(vec![
        Frame { flags: 2, ..frame(FrameMode::Verify, SENDER) },
        write(42),
        Frame { flags: 1, ..frame(FrameMode::Verify, payer) },
        write(99),
        frame(FrameMode::Assert, ASSERTION),
    ]));
    let result = evm.transact(&tx).unwrap().commit();
    assert!(!result.status);
    let outcome = result.frame.unwrap();
    assert_eq!(outcome.payer, payer);
    assert_eq!(outcome.assertion_failure, Some(4));
    assert_eq!(outcome.rolled_back, [false, false, false, true, false]);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::from(42));
}

#[rstest::rstest]
fn frames_respect_eip7702_delegation_activation(#[values(false, true)] enabled: bool) {
    let mut db = common::database_with_nonce_manager([
        (SENDER, APPROVE_BOTH),
        (ASSERTION, &[0x60, 42, 0x5f, 0x55, 0x00]),
    ]);
    db.insert_account_info(
        &BODY,
        AccountInfo::default().with_code(Bytecode::new_eip7702(ASSERTION)),
    );
    let mut evm = common::evm(db);
    let mut version = *evm.version();
    version.features.set(EvmFeatures::EIP7702, enabled);
    set_version(&mut evm, version);
    let tx =
        recovered(payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]));
    let result = evm.transact(&tx).unwrap().commit();
    assert_eq!(
        result.frame.unwrap().stops[1],
        if enabled { InstrStop::Stop } else { InstrStop::OpcodeNotFound },
    );
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::from(if enabled { 42 } else { 0 }));
}

#[test]
fn executable_prefix_discards_nonce_and_does_not_execute_body() {
    let mut evm = evm(&[0xfe], &[]);
    let tx =
        recovered(payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]));
    let prefix = evm.validate_frame_prefix(&tx, 1).unwrap();
    assert_eq!(prefix.payer, SENDER);
    assert!(prefix.sender_approved);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
    let result = evm.transact(&tx).unwrap().discard();
    assert_eq!(result.frame.unwrap().receipts[1].status, FrameStatus::Failure);
}

#[test]
fn keyed_nonce_updates_protocol_storage_without_bumping_account_nonce() {
    let mut evm = evm(&[], &[]);
    let mut tx = payload(vec![frame(FrameMode::Verify, SENDER)]);
    tx.nonce_keys = Some(vec![U256::from(7), U256::from(9)]);
    let _ = evm.transact(&recovered(tx)).unwrap().commit();
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
    for key in [7, 9] {
        assert_eq!(
            stored(&mut evm, NONCE_MANAGER, nonce_slot(SENDER, U256::from(key))),
            U256::from(1)
        );
    }
}

#[test]
fn empty_account_verification_uses_canonical_transaction_signature() {
    let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let sender =
        Address::from_raw_public_key(&key.verifying_key().to_sec1_point(false).as_bytes()[1..]);
    let mut tx = payload(vec![frame(FrameMode::Verify, sender)]);
    tx.sender = sender;
    tx.signatures.push(FrameSignature {
        scheme: SignatureScheme::Secp256k1,
        signature: vec![0; 65].into(),
        ..Default::default()
    });
    let (sig, recid) = key.sign_prehash_recoverable(tx.signature_hash().as_slice());
    let mut bytes = vec![recid.to_byte()];
    bytes.extend_from_slice(sig.to_bytes().as_slice());
    tx.signatures[0].signature = bytes.into();
    let mut evm = evm(&[], &[]);
    evm.overlay_db_mut().insert_account_info(
        &sender,
        AccountInfo::default().with_balance(U256::from(1_000_000_000)),
    );
    let result = evm.transact(&recovered(tx)).unwrap().commit();
    assert_eq!(result.frame.unwrap().payer, sender);
    assert_eq!(evm.state_mut().account(&sender).unwrap().nonce(), 1);
}

#[test]
fn intrinsic_reservation_matches_locked_shared_schedule() {
    let mut tx = payload(vec![frame(FrameMode::Verify, SENDER)]);
    tx.frames[0].data = vec![0, 1, 2, 0].into();
    let mut version = evm2::Version::new(SpecId::BOGOTA);
    version.gas_params.set(evm2::version::GasId::TxTokenCost, 1);
    version.gas_params.set(evm2::version::GasId::TxFloorCostPerToken, 16);
    version.gas_params.set(evm2::version::GasId::TxFloorZeroByteMultiplier, 4);
    let gas = FrameGas::calculate(&tx, &version).unwrap();
    assert_eq!(
        gas.intrinsic,
        tx.as_frame_ref().calculate_execution_gas_limit_with_token_cost(1)
            - tx.total_frame_execution_gas_limit()
    );
    assert_eq!(gas.floor, tx.as_frame_ref().calculate_calldata_floor());
    assert_eq!(gas.maximum, gas.execution + gas.state);
}

#[test]
fn restoration_does_not_fund_a_later_frame() {
    let mut evm = evm(&[0x5f, 0x35, 0x5f, 0x55, 0x00], &[]);
    let mut write = frame(FrameMode::Sender, BODY);
    write.data = U256::from(1).to_be_bytes::<32>().to_vec().into();
    let mut restore = frame(FrameMode::Sender, BODY);
    restore.limits.state = 0;
    let mut unfunded = write.clone();
    unfunded.limits.state = 0;
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER), write, restore, unfunded]));
    let result = evm.transact(&tx).unwrap().commit();
    let frames = result.frame.unwrap();
    assert_eq!(frames.receipts[1].gas_used.state, 0);
    assert_eq!(frames.receipts[2].status, FrameStatus::Success);
    assert_eq!(frames.receipts[3].status, FrameStatus::Failure);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::ZERO);
}

#[test]
fn reverted_restoration_preserves_the_original_owners_state_charge() {
    let code =
        [0x5f, 0x35, 0x5f, 0x55, 0x60, 0x20, 0x35, 0x60, 0x0b, 0x57, 0x00, 0x5b, 0x5f, 0x5f, 0xfd];
    let mut evm = evm(&code, &[]);
    let mut write = frame(FrameMode::Sender, BODY);
    write.data = U256::from(1).to_be_bytes::<32>().to_vec().into();
    let mut restore = frame(FrameMode::Sender, BODY);
    let mut data = vec![0; 64];
    data[63] = 1;
    restore.data = data.into();
    restore.limits.state = 0;
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER), write, restore]));
    let result = evm.transact(&tx).unwrap().commit();
    let frames = result.frame.unwrap();
    assert!(frames.receipts[1].gas_used.state > 0);
    assert_eq!(frames.receipts[2].status, FrameStatus::Failure);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::from(1));
}

#[test]
fn atomic_failure_preserves_status_and_skips_the_linked_suffix() {
    let mut evm = evm(&[0x60, 1, 0x5f, 0x55, 0x00], &[0x5f, 0x5f, 0xfd]);
    let mut write = frame(FrameMode::Sender, BODY);
    write.flags = 4;
    let mut fail = frame(FrameMode::Sender, ASSERTION);
    fail.flags = 4;
    let tx = recovered(payload(vec![
        frame(FrameMode::Verify, SENDER),
        write,
        fail,
        frame(FrameMode::Sender, BODY),
    ]));
    let result = evm.transact(&tx).unwrap().commit();
    let frames = result.frame.unwrap();
    assert_eq!(
        frames.receipts.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![
            FrameStatus::Success,
            FrameStatus::Success,
            FrameStatus::Failure,
            FrameStatus::SkippedAtomicBatch
        ]
    );
    assert_eq!(frames.receipts[1].gas_used.state, 0);
    assert!(frames.rolled_back[1]);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::ZERO);
}

#[test]
fn insufficient_value_pays_only_entry_access() {
    let mut evm = evm(&[], &[]);
    let mut transfer = frame(FrameMode::Sender, BODY);
    transfer.value = U256::MAX;
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER), transfer]));
    let result = evm.transact(&tx).unwrap().discard();
    let frames = result.frame.unwrap();
    assert_eq!(frames.stops[1], InstrStop::OutOfFunds);
    assert_eq!(frames.receipts[1].gas_used.execution, 3_000);
    assert_eq!(frames.receipts[1].gas_used.state, 0);
}

#[test]
fn prefix_stops_before_the_requested_body_even_without_a_payer() {
    let mut evm = evm(&[0xfe], &[]);
    let mut approve_execution = frame(FrameMode::Verify, SENDER);
    approve_execution.flags = 2;
    evm.overlay_db_mut().insert_account_info(
        &SENDER,
        AccountInfo::default()
            .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 2, 0x5f, 0x5f, 0xaa]))),
    );
    let tx = recovered(payload(vec![approve_execution, frame(FrameMode::Sender, BODY)]));
    assert!(evm.validate_frame_prefix(&tx, 1).is_err());
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
}

#[test]
fn aggregate_fee_signature_hash_preserves_full_width_budget_and_priority() {
    let mut tx = payload(vec![frame(FrameMode::Verify, SENDER)]);
    let large = U256::from(1) << 180;
    let baseline = tx.signature_hash();
    tx.fees.max_fee = large;
    let budget = tx.signature_hash();
    assert_ne!(budget, baseline);
    tx.fees.max_priority_fee_per_gas = large;
    assert_ne!(tx.signature_hash(), budget);
}

#[test]
fn recent_root_runtime_uses_actual_slot_and_the_locked_window() {
    assert_eq!(alloy_primitives::keccak256(&RECENT_ROOT_CODE), RECENT_ROOT_CODE_HASH);
    for (age, valid) in [(0, false), (1, true), (8191, true), (8192, false)] {
        let mut evm = evm(&[], &[]);
        let current_slot = 10_000u64;
        let slot = current_slot - age;
        let source = [0x11; 32];
        let root = [0x22; 32];
        let tuple = recent_tuple(source, slot, root);
        let value = recent_entry(&tuple);
        let key = recent_slot(source, slot);
        install_recent_root(&mut evm);
        evm.overlay_db_mut().insert_account_storage(&RECENT_ROOT_ADDRESS, &key, &value);
        evm.set_block(BlockEnvExt {
            slot_num: U256::from(current_slot),
            timestamp: U256::from(123),
            ..*evm.block()
        });
        let mut verifier = frame(FrameMode::Verify, RECENT_ROOT_ADDRESS);
        verifier.flags = 0;
        verifier.data = tuple.into();
        let tx = recovered(payload(vec![verifier, frame(FrameMode::Verify, SENDER)]));
        assert_eq!(
            evm.transact(&tx).map(|executed| executed.discard()).is_ok(),
            valid,
            "age {age}"
        );
    }
}

#[test]
fn bal_positioned_frames_preserve_prefix_writes_after_assertion_failure() {
    let body = [0x5f, 0x35, 0x5f, 0x55, 0x00];
    let assertion = [0x5f, 0x5f, 0xfd];
    let mut transactions = Vec::new();
    for nonce in 0..2 {
        let mut write = frame(FrameMode::Sender, BODY);
        write.data = U256::from(42 - nonce).to_be_bytes::<32>().to_vec().into();
        let mut tx = payload(vec![frame(FrameMode::Verify, SENDER), write]);
        tx.nonce = nonce;
        tx.nonce_keys = Some(vec![U256::from(7)]);
        if nonce == 1 {
            tx.frames.push(frame(FrameMode::Assert, ASSERTION));
        }
        transactions.push(recovered(tx));
    }
    let mut serial = evm(&body, &assertion);
    serial.state_mut().enable_bal_builder();
    serial.state_mut().reset_bal_index();
    let mut expected = Vec::new();
    for tx in &transactions {
        serial.state_mut().bump_bal_index();
        expected.push(serial.transact(tx).unwrap().commit());
    }
    assert!(!expected[1].status);
    let bal = Arc::new(serial.state_mut().take_bal_builder().unwrap());
    let mut rebuilt = Bal::new();
    for (index, tx) in transactions.iter().enumerate() {
        let index = BlockAccessIndex::new(index as u64 + 1);
        let mut positioned = evm(&body, &assertion);
        positioned.state_mut().set_bal(bal.clone());
        positioned.state_mut().set_bal_index(index);
        let detached = positioned.transact(tx).unwrap().detach();
        assert_eq!(detached.result, expected[index.get() as usize - 1]);
        rebuilt.commit(index, detached.pending_state);
    }
    assert_eq!(rebuilt, *bal);

    let slot = nonce_slot(SENDER, U256::from(7));
    let mut incomplete = (*bal).clone();
    assert!(
        incomplete
            .accounts
            .get_mut(&NONCE_MANAGER)
            .unwrap()
            .storage
            .storage
            .remove(&slot)
            .is_some()
    );
    let mut positioned = evm(&body, &assertion);
    positioned.state_mut().set_bal(Arc::new(incomplete));
    positioned.state_mut().set_bal_index(BlockAccessIndex::new(1));
    let error = positioned.transact(&transactions[0]).map(evm2::ExecutedTx::discard).unwrap_err();
    let HandlerError::Database(error) = error else { panic!("expected BAL input error") };
    assert!(!error.is_fatal());
    assert_eq!(
        error.downcast_ref::<BalError>(),
        Some(&BalError::SlotNotFound { address: NONCE_MANAGER, slot })
    );

    assert_eq!(
        stored(&mut serial, NONCE_MANAGER, nonce_slot(SENDER, U256::from(7))),
        U256::from(2)
    );
    assert_eq!(stored(&mut serial, BODY, U256::ZERO), U256::from(42));
}

#[test]
fn atomic_group_cannot_end_on_an_assertion() {
    let mut evm = evm(&[], &[]);
    let mut body = frame(FrameMode::Sender, BODY);
    body.flags = 4;
    let tx = recovered(payload(vec![
        frame(FrameMode::Verify, SENDER),
        body,
        frame(FrameMode::Assert, ASSERTION),
    ]));
    assert!(evm.transact(&tx).is_err());
    let valid = recovered(payload(vec![frame(FrameMode::Verify, SENDER)]));
    assert!(evm.transact(&valid).unwrap().discard().status);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
}

#[test]
fn preparation_uses_the_active_execution_cap() {
    let mut evm = evm(&[], &[]);
    let mut version = *evm.version();
    version.tx_gas_limit_cap = 40_000_000;
    set_version(&mut evm, version);
    evm.set_block(BlockEnvExt { gas_limit: U256::from(50_000_000), ..*evm.block() });
    let mut verify = frame(FrameMode::Verify, SENDER);
    verify.limits.execution = 20_000_000;
    let tx = recovered(payload(vec![verify]));
    assert!(evm.transact(&tx).unwrap().discard().status);
}

#[test]
fn prefix_forces_interpreted_observation_and_restores_the_full_execution_runner() {
    let mut evm = evm(&[], &[]);
    let calls = Arc::new(AtomicUsize::new(0));
    evm.set_interpreter_runner(CountingRunner(calls.clone()));
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER)]));
    evm.validate_frame_prefix(&tx, 1).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(evm.transact(&tx).unwrap().discard().status);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

struct PanicAtFrameEnd;

impl evm2::Inspector<BaseEvmTypes> for PanicAtFrameEnd {
    fn frame_end(
        &mut self,
        _: usize,
        _: &alloy_eips::eip8141::FrameReceipt,
        _: &Bytes,
        _: InstrStop,
        _: &mut Evm<'_, BaseEvmTypes>,
    ) {
        panic!("prefix inspector failed after approval");
    }
}

#[test]
fn prefix_unwind_discards_approved_nonce_and_restores_runner() {
    for borrowed in [false, true] {
        let mut evm = evm(&[], &[]);
        let calls = Arc::new(AtomicUsize::new(0));
        evm.set_interpreter_runner(CountingRunner(calls.clone()));
        if borrowed {
            evm.set_inspector(EntryFailureInspector::default());
        } else {
            evm.set_inspector(PanicAtFrameEnd);
        }
        let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER)]));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if borrowed {
                evm.validate_frame_prefix_with_inspector(&tx, 1, &mut PanicAtFrameEnd).unwrap();
            } else {
                evm.validate_frame_prefix(&tx, 1).unwrap();
            }
        }));
        assert!(panic.is_err());
        if borrowed {
            let inspector = evm.clear_inspector_as::<EntryFailureInspector>().unwrap();
            assert!(inspector.starts.is_empty());
        } else {
            evm.clear_inspector();
        }
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
        assert!(evm.transact(&tx).unwrap().discard().status);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn unused_malformed_signature_precedes_database_reads_and_fatal_errors_survive() {
    let mut evm = evm(&[], &[]);
    let reads = Arc::new(AtomicUsize::new(0));
    evm.set_database(Db::new(FailingDatabase(reads.clone(), "frame database failure")));
    let mut malformed = payload(vec![frame(FrameMode::Verify, SENDER)]);
    malformed.signatures.push(FrameSignature {
        scheme: SignatureScheme::Secp256k1,
        signature: Bytes::from_static(&[0]),
        ..Default::default()
    });
    assert!(matches!(
        evm.validate_frame_prefix(&recovered(malformed), 1),
        Err(evm2::registry::HandlerError::InvalidFrameTransaction(_))
    ));
    assert_eq!(reads.load(Ordering::Relaxed), 0);

    evm.overlay_db_mut().insert_account_info(
        &SENDER,
        AccountInfo::default()
            .with_balance(U256::from(1_000_000_000))
            .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 3, 0x5f, 0x5f, 0xaa]))),
    );
    let tx =
        recovered(payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]));
    assert!(
        matches!(evm.transact(&tx), Err(evm2::registry::HandlerError::Database(error)) if error.is_fatal())
    );
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
    evm.overlay_db_mut().insert_account_info(&Address::ZERO, AccountInfo::default());
    let valid = recovered(payload(vec![frame(FrameMode::Verify, SENDER)]));
    assert!(evm.transact(&valid).unwrap().discard().status);
}

#[test]
fn every_transaction_resolution_clears_frame_context_before_an_ordinary_transaction() {
    for resolution in ["commit", "discard", "detach", "drop", "invalid"] {
        let mut evm = evm(&[0x5f, 0xb0, 0x50, 0x00], &[]);
        let tx = recovered(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Sender, BODY),
        ]));
        match resolution {
            "commit" => {
                assert!(evm.transact(&tx).unwrap().commit().status);
            }
            "discard" => {
                assert!(evm.transact(&tx).unwrap().discard().status);
            }
            "detach" => {
                assert!(evm.transact(&tx).unwrap().detach().result.status);
            }
            "drop" => {
                drop(evm.transact(&tx).unwrap());
            }
            "invalid" => {
                assert!(evm.transact(&recovered(payload(Vec::new()))).is_err());
            }
            _ => unreachable!(),
        }
        let caller = Address::with_last_byte(0xd0);
        evm.overlay_db_mut().insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000)),
        );
        let normal = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 100_000,
                to: TxKind::Call(BODY),
                ..Default::default()
            }),
            caller,
        );
        let result = evm.transact(&normal).unwrap().discard();
        assert!(!result.status, "{resolution}");
        assert_eq!(result.stop, InstrStop::OpcodeNotFound, "{resolution}");
        assert!(result.frame.is_none(), "{resolution}");
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EntryFailureInspector {
    transactions_started: usize,
    transactions_ended: usize,
    starts: Vec<usize>,
    ends: Vec<(usize, FrameStatus, InstrStop)>,
}

impl<T: evm2::EvmTypes> evm2::Inspector<T> for EntryFailureInspector {
    fn frame_transaction_start(&mut self, _: &Arc<TxEip8141>, _: &FrameGas, _: &mut Evm<'_, T>) {
        self.transactions_started += 1;
    }

    fn frame_transaction_end(&mut self, _: &evm2::TxResult<T>, _: &mut Evm<'_, T>) {
        self.transactions_ended += 1;
    }

    fn frame_start(&mut self, index: usize, _: &Frame, _: &mut Evm<'_, T>) {
        self.starts.push(index);
    }

    fn frame_end(
        &mut self,
        index: usize,
        receipt: &alloy_eips::eip8141::FrameReceipt,
        _: &Bytes,
        stop: InstrStop,
        _: &mut Evm<'_, T>,
    ) {
        self.ends.push((index, receipt.status, stop));
    }
}

#[test]
fn verify_entry_failure_emits_its_frame_end_before_invalidating_the_prefix() {
    let mut evm = evm(&[], &[]);
    evm.set_inspector(EntryFailureInspector::default());
    let mut verify = frame(FrameMode::Verify, SENDER);
    verify.limits.execution = 0;
    assert!(evm.validate_frame_prefix(&recovered(payload(vec![verify])), 1).is_err());
    let inspector = evm.clear_inspector_as::<EntryFailureInspector>().unwrap();
    assert_eq!(inspector.starts, vec![0]);
    assert_eq!(inspector.ends, vec![(0, FrameStatus::Failure, InstrStop::OutOfGas)]);
}

#[test]
fn elided_witness_hash_and_intrinsic_commit_to_length_instead_of_contents() {
    let version = Version::base(SpecId::BOGOTA);
    let token = version.gas_params.get(GasId::TxTokenNonZeroByteMultiplier);
    let intrinsic_byte_cost = token * version.gas_params.get(GasId::TxTokenCost);
    let floor_byte_cost = token * version.gas_params.get(GasId::TxFloorCostPerToken);
    for len in [0, 1, 55, 56, 64, 128, 256] {
        let mut zero = payload(vec![frame(FrameMode::Verify, SENDER)]);
        zero.signatures
            .push(FrameSignature { signature: vec![0; len].into(), ..Default::default() });
        let mut nonzero = zero.clone();
        nonzero.signatures[0].signature = vec![0xff; len].into();
        let gas = FrameGas::calculate(&zero, version).unwrap();
        assert_eq!(FrameGas::calculate(&nonzero, version).unwrap(), gas, "length {len}");
        assert_eq!(nonzero.signature_hash(), zero.signature_hash(), "length {len}");
        nonzero.signatures[0].signature = vec![0xff; len + 1].into();
        assert_ne!(nonzero.signature_hash(), zero.signature_hash(), "length {len}");
        let extended = FrameGas::calculate(&nonzero, version).unwrap();
        assert_eq!(extended.intrinsic - gas.intrinsic, intrinsic_byte_cost);
        assert_eq!(extended.floor - gas.floor, floor_byte_cost);
    }
}

#[test]
fn explicit_signature_messages_preserve_content_sensitive_hashing_and_intrinsic_pricing() {
    let version = Version::base(SpecId::BOGOTA);
    let mut zero = payload(vec![frame(FrameMode::Verify, SENDER)]);
    zero.signatures.push(FrameSignature {
        msg: SignatureMessage::Explicit(B256::repeat_byte(1)),
        signature: vec![0; 64].into(),
        ..Default::default()
    });
    let mut nonzero = zero.clone();
    nonzero.signatures[0].signature = vec![0xff; 64].into();
    assert_ne!(nonzero.signature_hash(), zero.signature_hash());
    let zero_gas = FrameGas::calculate(&zero, version).unwrap();
    let nonzero_gas = FrameGas::calculate(&nonzero, version).unwrap();
    let token_difference = version.gas_params.get(GasId::TxTokenNonZeroByteMultiplier) - 1;
    assert_eq!(
        nonzero_gas.intrinsic - zero_gas.intrinsic,
        64 * token_difference * version.gas_params.get(GasId::TxTokenCost)
    );
    assert_eq!(nonzero_gas.floor, zero_gas.floor);
}

#[test]
fn aggregate_fee_escrow_settlement_and_gasprice_cover_full_partial_and_overflowing_tips() {
    let beneficiary = Address::with_last_byte(0xd0);
    for blobs in [false, true] {
        for kind in ["full", "partial", "required", "capped"] {
            let mut evm = evm(&[0x3a, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3], &[]);
            evm.set_block(BlockEnvExt {
                basefee: U256::from(7),
                blob_basefee: U256::from(3),
                beneficiary,
                ..Default::default()
            });
            let mut tx = payload(vec![
                frame(FrameMode::Verify, SENDER),
                frame(FrameMode::Sender, BODY),
                frame(FrameMode::Default, BODY),
                frame(FrameMode::Assert, BODY),
            ]);
            tx.nonce_keys = Some(vec![]);
            if blobs {
                tx.blob_versioned_hashes.push(B256::repeat_byte(1));
            }
            let gas = FrameGas::calculate(&tx, evm.version()).unwrap();
            let max_gas = U256::from(gas.maximum);
            let blob_charge = if blobs { U256::from(131_072 * 3) } else { U256::ZERO };
            let required = max_gas * U256::from(7) + blob_charge;
            let (requested_tip, headroom, reported_price) = match kind {
                "full" => (U256::from(11), max_gas * U256::from(11) + U256::from(99), 18),
                "partial" => (U256::from(11), max_gas * U256::from(3) + U256::ONE, 10),
                "required" => (U256::from(11), U256::ZERO, 7),
                _ => (U256::MAX, U256::from(17), 7),
            };
            tx.fees.max_fee = required + headroom;
            tx.fees.max_priority_fee_per_gas = requested_tip;
            let expected_escrow =
                if kind == "full" { required + max_gas * U256::from(11) } else { tx.fees.max_fee };
            let max_fee = tx.fees.max_fee;
            let result = evm.transact(&recovered(tx)).unwrap().commit();
            assert!(result.status, "{kind}, blobs={blobs}");
            let used = U256::from(result.tx_gas_used());
            let paid_base_fee = used * U256::from(7) + blob_charge;
            let expected_tip = if kind == "capped" {
                expected_escrow - paid_base_fee
            } else {
                used * U256::from(11)
            };
            assert!(paid_base_fee + expected_tip <= expected_escrow);
            assert!(expected_escrow <= max_fee);
            let frame = result.frame.unwrap();
            assert_eq!(frame.precharge, expected_escrow, "{kind}, blobs={blobs}");
            for output in &frame.outputs[1..] {
                assert_eq!(U256::from_be_slice(output), U256::from(reported_price));
            }
            assert_eq!(
                evm.state_mut().account(&SENDER).unwrap().balance(),
                U256::from(1_000_000_000) - paid_base_fee - expected_tip,
                "{kind}, blobs={blobs}"
            );
            assert_eq!(evm.state_mut().account(&beneficiary).unwrap().balance(), expected_tip);
            if matches!(kind, "partial" | "required") {
                // Unused gas backs the full requested tip, so GASPRICE is a strict lower bound.
                assert!(used * U256::from(reported_price) < used * U256::from(7) + expected_tip);
            }
        }
    }
}

#[test]
fn nonce_intrinsic_charges_zero_keys_and_exempts_only_single_use_sequences() {
    let version = Version::base(SpecId::BOGOTA);
    let gp = &version.gas_params;
    let token = gp.get(GasId::TxTokenNonZeroByteMultiplier);
    let intrinsic_byte_cost = token * gp.get(GasId::TxTokenCost);
    let floor_byte_cost = token * gp.get(GasId::TxFloorCostPerToken);
    let key_cost = gp.get(GasId::WarmStorageReadCost)
        + gp.get(GasId::ColdStorageAdditionalCost)
        + gp.get(GasId::SstoreSetWithoutLoadCost);
    let mut tx = payload(vec![frame(FrameMode::Verify, SENDER)]);
    tx.nonce_keys = Some(vec![]);
    let legacy = FrameGas::calculate(&tx, version).unwrap();
    tx.nonce_keys = Some(vec![U256::ZERO]);
    let zero_key = FrameGas::calculate(&tx, version).unwrap();
    assert_eq!(zero_key.intrinsic - legacy.intrinsic, key_cost + intrinsic_byte_cost);
    assert_eq!(zero_key.floor - legacy.floor, key_cost + floor_byte_cost);
    tx.nonce_keys = Some(vec![U256::ONE]);
    assert_eq!(FrameGas::calculate(&tx, version).unwrap(), zero_key);
    tx.nonce = u64::MAX - 1;
    let incrementable = FrameGas::calculate(&tx, version).unwrap();
    tx.nonce = u64::MAX;
    let single_use = FrameGas::calculate(&tx, version).unwrap();
    // The ordinary sequence encodes as 0x88 followed by eight nonzero bytes.
    assert_eq!(incrementable.intrinsic - single_use.intrinsic, 9 * intrinsic_byte_cost);
    assert_eq!(incrementable.floor - single_use.floor, 9 * floor_byte_cost);
}

#[rstest::rstest]
fn disabling_base_fee_checks_keeps_charging_within_budget(
    #[values(0, 1, 1_000_000_000)] budget: u64,
) {
    let mut evm = evm(&[0x3a, 0x5f, 0x52, 0x48, 0x60, 32, 0x52, 0x60, 64, 0x5f, 0xf3], &[]);
    let beneficiary = Address::with_last_byte(0xd0);
    let base_fee = U256::from(7);
    let tip = U256::from(2);
    evm.set_block(BlockEnvExt { basefee: base_fee, beneficiary, ..Default::default() });
    let mut payload =
        payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]);
    payload.fees.max_fee = U256::from(budget);
    payload.fees.max_priority_fee_per_gas = tip;
    let max_gas = U256::from(FrameGas::calculate(&payload, evm.version()).unwrap().maximum);
    let escrow = U256::from(budget).min(max_gas * (base_fee + tip));
    let tx = recovered(payload);
    assert_eq!(evm.validate_tx(&tx).is_ok(), U256::from(budget) >= max_gas * base_fee);
    let mut version = *evm.version();
    version.features.remove(EvmFeatures::BASE_FEE_CHECK);
    set_version(&mut evm, version);
    let result = evm.transact(&tx).unwrap().commit();
    assert!(result.status);
    let used = U256::from(result.tx_gas_used());
    let base_paid = (used * base_fee).min(escrow);
    let tip_paid = (used * tip).min(escrow - base_paid);
    let frame = result.frame.unwrap();
    assert_eq!(frame.precharge, escrow);
    assert_eq!(frame.outputs[1].len(), 64);
    assert_eq!(U256::from_be_slice(&frame.outputs[1][..32]), escrow / max_gas);
    assert_eq!(U256::from_be_slice(&frame.outputs[1][32..]), base_fee);
    assert_eq!(evm.state_mut().account(&beneficiary).unwrap().balance(), tip_paid);
    assert_eq!(
        evm.state_mut().account(&SENDER).unwrap().balance(),
        U256::from(1_000_000_000) - base_paid - tip_paid
    );
}

#[test]
fn custom_refund_quotients_keep_prefix_and_full_execution_consistent() {
    let tx =
        recovered(payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]));
    let mut expected_prefix = None;
    for quotient in [1, 2, 5] {
        let mut evm = evm(&[0x5f, 0x5f, 0x55, 0x00], &[]);
        evm.overlay_db_mut().insert_account_storage(&BODY, &U256::ZERO, &U256::ONE);
        let mut version = *evm.version();
        version.gas_params.set(GasId::MaxRefundQuotient, quotient);
        set_version(&mut evm, version);
        let prefix = evm.validate_frame_prefix(&tx, 1).unwrap();
        assert_eq!(prefix, *expected_prefix.get_or_insert(prefix));
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
        let result = evm.transact(&tx).unwrap().commit();
        assert!(result.status);
        let available_refund = version.gas_params.get(GasId::SstoreClearingSlotRefund);
        assert_eq!(
            result.refunded,
            available_refund.min(result.total_gas_spent / u64::from(quotient))
        );
        assert!(result.refunded > 0);
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
        assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::ZERO);
    }
}

#[test]
fn payment_approval_honors_balance_top_up_independently_of_fee_and_balance_checks() {
    for fee_charge in [false, true] {
        for balance_check in [false, true] {
            for balance_top_up in [false, true] {
                for initial_balance in [U256::from(7), U256::from(1_000_000_000)] {
                    let label = format!(
                        "fee_charge={fee_charge}, balance_check={balance_check}, balance_top_up={balance_top_up}, balance={initial_balance}"
                    );
                    // The sender pays for this transaction. Read its balance after APPROVE,
                    // before settlement, so later refunds cannot hide an unwanted top-up.
                    let mut evm = evm(&[0x33, 0x31, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3], &[]);
                    let beneficiary = Address::with_last_byte(0xd0);
                    evm.set_block(BlockEnvExt { beneficiary, ..Default::default() });
                    let sender = evm
                        .database_as_mut::<InMemoryDB>()
                        .unwrap()
                        .cache
                        .accounts
                        .get_mut(&SENDER)
                        .unwrap();
                    sender.as_mut().unwrap().balance = initial_balance;
                    let mut version = *evm.version();
                    for (feature, enabled) in [
                        (EvmFeatures::FEE_CHARGE, fee_charge),
                        (EvmFeatures::BALANCE_CHECK, balance_check),
                        (EvmFeatures::BALANCE_TOP_UP, balance_top_up),
                    ] {
                        version.features.set(feature, enabled);
                    }
                    set_version(&mut evm, version);
                    let mut payload = payload(vec![
                        frame(FrameMode::Verify, SENDER),
                        frame(FrameMode::Sender, BODY),
                    ]);
                    payload.fees.max_fee = U256::from(1_000_000_000);
                    payload.fees.max_priority_fee_per_gas = U256::ONE;
                    let escrow =
                        U256::from(FrameGas::calculate(&payload, &version).unwrap().maximum);
                    let tx = recovered(payload);
                    let prefix = evm.validate_frame_prefix(&tx, 1);
                    assert_eq!(
                        evm.state_mut().account(&SENDER).unwrap().balance(),
                        initial_balance
                    );
                    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
                    if balance_check && initial_balance < escrow {
                        assert_eq!(
                            prefix.unwrap_err(),
                            HandlerError::InvalidFrameTransaction(
                                FrameTransactionError::VerifyFrameFailed
                            ),
                            "{label}"
                        );
                        assert_eq!(
                            evm.transact(&tx).unwrap_err(),
                            HandlerError::InvalidFrameTransaction(
                                FrameTransactionError::VerifyFrameFailed
                            ),
                            "{label}"
                        );
                        assert_eq!(
                            evm.state_mut().account(&SENDER).unwrap().balance(),
                            initial_balance
                        );
                        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
                        continue;
                    }
                    assert_eq!(prefix.unwrap().max_cost, escrow, "{label}");
                    let result = evm.transact(&tx).unwrap().commit();
                    assert!(result.status, "{label}");
                    let frame = result.frame.as_ref().unwrap();
                    let funded_balance = if !balance_check && balance_top_up {
                        initial_balance.max(escrow)
                    } else {
                        initial_balance
                    };
                    let expected_precharge =
                        if fee_charge { funded_balance.min(escrow) } else { U256::ZERO };
                    let expected_balance = funded_balance - expected_precharge;
                    assert_eq!(U256::from_be_slice(&frame.outputs[1]), expected_balance, "{label}");
                    assert_eq!(frame.precharge, expected_precharge, "{label}");
                    let tip_paid = U256::from(result.tx_gas_used()).min(expected_precharge);
                    let final_balance = funded_balance - tip_paid;
                    assert_eq!(
                        evm.state_mut().account(&beneficiary).unwrap().balance(),
                        tip_paid,
                        "{label}"
                    );
                    assert_eq!(
                        evm.state_mut().account(&SENDER).unwrap().balance(),
                        final_balance,
                        "{label}"
                    );
                    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
                }
            }
        }
    }
}

struct FrameEnvelope(TxEip8141);

impl Typed2718 for FrameEnvelope {
    fn ty(&self) -> u8 {
        6
    }
}

struct FrameTypes;

impl EvmTypesHost for FrameTypes {
    type ConfigSelector = BaseEvmConfigSelector;
    type SpecId = SpecId;
    type Tx = FrameEnvelope;
    type EvmExt = ();
    type MessageExt = ();
    type MessageResultExt = ();
    type TxEnvExt = ();
    type TxResultExt = ();
    type BlockEnvExt = ();
    type Host<'a> = Evm<'a, Self>;
}

#[test]
fn native_frame_prefix_dispatches_for_custom_transaction_envelopes() {
    let registry = TxRegistry::<FrameTypes, _>::new().with_handler(
        6,
        |tx: &FrameEnvelope| Some(&tx.0),
        FrameHandler::<DefaultTxHandlerHooks>::new(),
    );
    let mut evm = Evm::new(
        SpecId::BOGOTA,
        BlockEnvExt::default(),
        registry,
        common::database([(SENDER, APPROVE_BOTH), (BODY, &[0xfe])]),
        Precompiles::base(SpecId::BOGOTA),
    );
    let tx = Recovered::new_unchecked(
        FrameEnvelope(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Sender, BODY),
        ])),
        Address::ZERO,
    );
    evm.set_inspector(EntryFailureInspector::default());
    let mut inspector = EntryFailureInspector::default();
    let prefix = evm.validate_frame_prefix_with_inspector(&tx, 1, &mut inspector).unwrap();
    assert_eq!(prefix.payer, SENDER);
    assert!(prefix.sender_approved);
    assert_eq!(prefix.prefix_end, 1);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
    assert_eq!(inspector.starts, vec![0]);
    let previous = evm.clear_inspector_as::<EntryFailureInspector>().unwrap();
    assert!(previous.starts.is_empty());
    let result = evm.transact_with_inspector(&tx, &mut inspector).unwrap().discard();
    assert_eq!(result.frame.unwrap().receipts[1].status, FrameStatus::Failure);
    assert_eq!(inspector.starts, vec![0, 0, 1]);
    assert!(evm.inspector().is_none());
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
}

#[test]
fn unsuccessful_prefix_frames_report_the_revm_validation_error() {
    for body in [&[0x5f, 0x5f, 0xfd][..], &[0xfe][..]] {
        let mut evm = evm(body, &[]);
        let tx = recovered(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Default, BODY),
        ]));
        let error = evm.validate_frame_prefix(&tx, 2).unwrap_err();
        assert_eq!(
            error,
            HandlerError::InvalidFrameTransaction(FrameTransactionError::UnsuccessfulPrefixFrame)
        );
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
    }
}

#[test]
fn failed_batch_does_not_execute_past_the_prefix_boundary() {
    let mut evm = evm(&[0x5f, 0x5f, 0xfd], &[]);
    let tx = recovered(payload(vec![
        Frame { flags: 4, ..frame(FrameMode::Default, BODY) },
        frame(FrameMode::Default, BODY),
        frame(FrameMode::Verify, SENDER),
    ]));
    let mut inspector = EntryFailureInspector::default();
    let error = evm.validate_frame_prefix_with_inspector(&tx, 1, &mut inspector).unwrap_err();
    assert_eq!(
        error,
        HandlerError::InvalidFrameTransaction(FrameTransactionError::UnsuccessfulPrefixFrame)
    );
    assert_eq!(inspector.starts, vec![0]);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
}

#[test]
fn frame_handler_supports_direct_execution_and_prefix_validation() {
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER)]));
    let mut execution_evm = evm(&[], &[]);
    let handler = execution_evm.registry().try_get_by_type(tx.ty()).unwrap();
    let result = handler.execute(tx.as_recovered_ref(), &mut execution_evm).unwrap();
    assert_eq!(result.frame.unwrap().payer, SENDER);

    let mut prefix_evm = evm(&[], &[]);
    let prefix = handler.validate_frame_prefix(tx.as_recovered_ref(), &mut prefix_evm, 1).unwrap();
    assert_eq!(prefix.payer, SENDER);
    assert!(prefix.sender_approved);
}

mod handler_hooks {
    use super::*;
    use evm2::{
        ExecutionConfig, TxResult, ethereum::eip8141::PreparedFrameTx,
        evm::handler::TxHandlerHooks, registry::HandlerResult,
    };
    use std::cell::RefCell;

    thread_local! {
        static CALLS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    #[derive(Debug, thiserror::Error, PartialEq, Eq)]
    enum HookError {
        #[error("preparation hook failed")]
        Preparation,
        #[error("settlement hook failed")]
        Settlement,
    }

    struct Hooks<const FAIL: u8>;

    impl<const FAIL: u8> TxHandlerHooks<BaseEvmTypes> for Hooks<FAIL> {
        fn before_frame_execution(
            host: &mut Evm<'_, BaseEvmTypes>,
            _envelope: &TxEnvelope,
            _prepared: &PreparedFrameTx,
        ) -> HandlerResult<()> {
            CALLS.with_borrow_mut(|calls| calls.push("prepare"));
            let sender = host.state_mut().account(&SENDER)?;
            assert_eq!(sender.nonce(), 0);
            assert_eq!(sender.balance(), U256::from(1_000_000_000));
            if FAIL == 1 {
                return Err(HandlerError::external(HookError::Preparation));
            }
            Ok(())
        }

        fn settle_frame_transaction(
            host: &mut Evm<'_, BaseEvmTypes>,
            envelope: &TxEnvelope,
            prepared: &PreparedFrameTx,
            result: TxResult,
        ) -> HandlerResult<TxResult> {
            CALLS.with_borrow_mut(|calls| calls.push("settle"));
            assert_eq!(host.state_mut().account(&SENDER)?.nonce(), 1);
            if FAIL == 2 {
                return Err(HandlerError::external(HookError::Settlement));
            }
            DefaultTxHandlerHooks::settle_frame_transaction(host, envelope, prepared, result)
        }
    }

    fn evm<const FAIL: u8>() -> Evm<'static, BaseEvmTypes> {
        let mut evm = super::evm(&[], &[]);
        let registry = TxRegistry::new().with_handler(
            6,
            TxEnvelope::as_eip8141,
            FrameHandler::<Hooks<FAIL>>::new(),
        );
        evm.set_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::BOGOTA, *evm.version()),
            SpecId::BOGOTA,
            registry,
            Precompiles::base(SpecId::BOGOTA),
        );
        evm.set_block(BlockEnvExt { basefee: U256::ONE, ..Default::default() });
        evm
    }

    fn transaction() -> Recovered<TxEnvelope> {
        let mut tx =
            payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]);
        tx.fees.max_fee = U256::from(10_000_000);
        tx.fees.max_priority_fee_per_gas = U256::from(2);
        recovered(tx)
    }

    #[test]
    fn validation_and_prefix_share_preparation_without_settlement() {
        let mut evm = evm::<0>();
        let tx = transaction();
        CALLS.with_borrow_mut(Vec::clear);
        evm.validate_tx(&tx).unwrap();
        assert_eq!(CALLS.with_borrow(Clone::clone), ["prepare"]);
        assert_eq!(evm.validate_frame_prefix(&tx, 1).unwrap().payer, SENDER);
        assert_eq!(CALLS.with_borrow(Clone::clone), ["prepare", "prepare"]);
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().balance(), U256::from(1_000_000_000));

        let result = evm.transact(&tx).unwrap().commit();
        assert_eq!(CALLS.with_borrow(Clone::clone), ["prepare", "prepare", "prepare", "settle"]);
        let mut standard = super::evm(&[], &[]);
        standard.set_block(BlockEnvExt { basefee: U256::ONE, ..Default::default() });
        let expected = standard.transact(&tx).unwrap().commit();
        assert_eq!(result.tx_gas_used(), expected.tx_gas_used());
        assert_eq!(result.frame, expected.frame);
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
        assert_eq!(
            evm.state_mut().account(&SENDER).unwrap().balance(),
            standard.state_mut().account(&SENDER).unwrap().balance(),
        );
    }

    #[rstest::rstest]
    #[case(1, HookError::Preparation, vec!["prepare"])]
    #[case(2, HookError::Settlement, vec!["prepare", "settle"])]
    fn hook_errors_discard_approval_and_fees(
        #[case] stage: u8,
        #[case] expected: HookError,
        #[case] calls: Vec<&'static str>,
    ) {
        let mut evm = if stage == 1 { evm::<1>() } else { evm::<2>() };
        let tx = transaction();
        CALLS.with_borrow_mut(Vec::clear);
        let error = evm.transact(&tx).unwrap_err();
        assert_eq!(error.external_ref::<HookError>(), Some(&expected));
        assert_eq!(CALLS.with_borrow(Clone::clone), calls);
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().balance(), U256::from(1_000_000_000));
    }
}

#[test]
fn gas_reservations_accept_the_u64_boundary_and_reject_one_more() {
    let mut version = Version::new(SpecId::BOGOTA);
    version.gas_params.set(GasId::TxTokenCost, 0);
    version.gas_params.set(GasId::TxFloorCostPerToken, 0);
    for execution in [0, 7, u64::MAX - 12_475] {
        let mut tx = payload(vec![frame(FrameMode::Verify, SENDER)]);
        tx.frames[0].limits.execution = execution;
        tx.frames[0].limits.state = u64::MAX - 12_475 - execution;
        let gas = FrameGas::calculate(&tx, &version).unwrap();
        assert_eq!(gas.intrinsic, 12_475);
        assert_eq!(gas.execution, 12_475 + execution);
        assert_eq!(gas.maximum, u64::MAX);
        for state in [false, true] {
            let mut overflow = tx.clone();
            if state {
                overflow.frames[0].limits.state += 1;
            } else {
                overflow.frames[0].limits.execution += 1;
            }
            assert_eq!(
                FrameGas::calculate(&overflow, &version),
                Err(HandlerError::InvalidFrameTransaction(FrameTransactionError::GasLimitOverflow)),
            );
        }
    }
}

#[test]
fn saturated_frame_budget_sums_are_rejected_even_with_zero_data_prices() {
    let mut version = Version::new(SpecId::BOGOTA);
    version.gas_params.set(GasId::TxTokenCost, 0);
    version.gas_params.set(GasId::TxFloorCostPerToken, 0);
    for state in [false, true] {
        let mut tx = payload(vec![frame(FrameMode::Verify, SENDER); 2]);
        for (frame, limit) in tx.frames.iter_mut().zip([u64::MAX, 1]) {
            frame.limits = Default::default();
            if state {
                frame.limits.state = limit;
            } else {
                frame.limits.execution = limit;
            }
        }
        assert_eq!(
            FrameGas::calculate(&tx, &version),
            Err(HandlerError::InvalidFrameTransaction(FrameTransactionError::GasLimitOverflow)),
        );
    }
}

#[test]
fn custom_data_weights_price_zero_bytes_and_reject_cost_overflow() {
    let mut version = Version::new(SpecId::BOGOTA);
    version.gas_params.set(GasId::TxTokenCost, 3);
    version.gas_params.set(GasId::TxFloorCostPerToken, 11);
    version.gas_params.set(GasId::TxTokenNonZeroByteMultiplier, 7);
    version.gas_params.set(GasId::TxFloorZeroByteMultiplier, 5);
    let mut tx = payload(vec![frame(FrameMode::Verify, SENDER)]);
    tx.frames[0].data = vec![0, 1, 2, 0].into();
    tx.frames[0].limits = Default::default();
    let gas = FrameGas::calculate(&tx, &version).unwrap();
    // The two nonce bytes and frame data contain four nonzero bytes and two zero bytes.
    assert_eq!(gas.intrinsic, 12_565);
    assert_eq!(gas.floor, 12_893);
    assert_eq!(gas.maximum, 12_893);
    for multiplier in [GasId::TxTokenNonZeroByteMultiplier, GasId::TxFloorZeroByteMultiplier] {
        let mut overflow = version;
        overflow.gas_params.set(multiplier, u32::MAX);
        overflow.gas_params.set(GasId::TxTokenCost, u32::MAX);
        overflow.gas_params.set(GasId::TxFloorCostPerToken, u32::MAX);
        assert_eq!(
            FrameGas::calculate(&tx, &overflow),
            Err(HandlerError::InvalidFrameTransaction(FrameTransactionError::GasLimitOverflow)),
        );
        overflow.gas_params.set(GasId::TxTokenCost, 0);
        overflow.gas_params.set(GasId::TxFloorCostPerToken, 0);
        assert_eq!(FrameGas::calculate(&tx, &overflow).unwrap().maximum, 12_475);
    }
}

#[test]
fn tuple_and_optional_inspectors_forward_frame_lifecycle_hooks() {
    let mut evm = evm(&[], &[]);
    evm.set_inspector((
        Some(EntryFailureInspector::default()),
        (None::<EntryFailureInspector>, EntryFailureInspector::default()),
    ));
    let tx =
        recovered(payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Sender, BODY)]));
    assert!(evm.transact(&tx).unwrap().commit().status);
    let (left, (absent, right)) = *evm.clear_inspector_as::<(
        Option<EntryFailureInspector>,
        (Option<EntryFailureInspector>, EntryFailureInspector),
    )>().unwrap();
    assert!(absent.is_none());
    assert_eq!(left.as_ref(), Some(&right));
    assert_eq!(right.transactions_started, 1);
    assert_eq!(right.transactions_ended, 1);
    assert_eq!(right.starts, vec![0, 1]);
    assert_eq!(right.ends.len(), 2);
    assert!(right.ends.iter().all(|(_, status, _)| *status == FrameStatus::Success));
}
