//! Approval, rollback, async cancellation, and transaction-owner release regressions.

mod common;

use alloy_consensus::TxEip8141;
use alloy_eips::{
    eip8141::{
        Frame, FrameLimits, FrameMode, FrameSignature, FrameStatus, SignatureMessage,
        SignatureScheme,
    },
    eip8250::{NONCE_MANAGER, nonce_slot},
};
use alloy_primitives::{Address, B256, U256};
use common::{APPROVE_BOTH, BODY, SENDER, recovered, set_version, stored};
use evm2::{
    BaseEvmTypes, Evm, EvmFeatures, SpecId,
    bytecode::Bytecode,
    evm::AccountInfo,
    interpreter::InstrStop,
    registry::{FrameTransactionError, HandlerError},
    version::GasId,
};

const FAILURE: Address = Address::with_last_byte(0xc0);

fn frame(mode: FrameMode, target: Address, flags: u8) -> Frame {
    Frame {
        flags,
        limits: FrameLimits { execution: 150_000, state: 300_000 },
        ..common::frame(mode, target)
    }
}

fn payload(frames: Vec<Frame>, keys: Vec<U256>) -> TxEip8141 {
    TxEip8141 { nonce_keys: Some(keys), ..common::payload(frames) }
}

fn evm(sender: &[u8], body: &[u8]) -> Evm<'static, BaseEvmTypes> {
    let db = common::database_with_nonce_manager([
        (SENDER, sender),
        (BODY, body),
        (FAILURE, &[0x5f, 0x5f, 0xfd]),
    ]);
    common::evm(db)
}

fn nonce(evm: &mut Evm<'_, BaseEvmTypes>, key: U256) -> U256 {
    if key.is_zero() {
        U256::from(evm.state_mut().account(&SENDER).unwrap().nonce())
    } else {
        stored(evm, NONCE_MANAGER, nonce_slot(SENDER, key))
    }
}

fn set_placeholders(evm: &mut Evm<'_, BaseEvmTypes>, enabled: bool) {
    let mut version = *evm.version();
    version.features.set(EvmFeatures::FRAME_SIGNATURE_PLACEHOLDERS, enabled);
    set_version(evm, version);
}

#[test]
fn reserved_zero_signature_message_is_rejected_even_with_placeholders() {
    for placeholders in [false, true] {
        for scheme in
            [SignatureScheme::Arbitrary, SignatureScheme::Secp256k1, SignatureScheme::P256]
        {
            let mut evm = evm(APPROVE_BOTH, &[]);
            set_placeholders(&mut evm, placeholders);
            let mut tx = payload(vec![frame(FrameMode::Verify, SENDER, 3)], vec![]);
            tx.signatures.push(FrameSignature {
                scheme,
                msg: SignatureMessage::Explicit(B256::ZERO),
                ..Default::default()
            });
            let tx = recovered(tx);
            common::assert_validation_error(
                &mut evm,
                &tx,
                HandlerError::InvalidFrameTransaction(
                    FrameTransactionError::InvalidSignatureMessage,
                ),
            );
            assert_eq!(nonce(&mut evm, U256::ZERO), U256::ZERO);
        }
    }
}

#[test]
fn nonzero_arbitrary_message_and_unsigned_simulation_placeholders_remain_valid() {
    for scheme in [SignatureScheme::Arbitrary, SignatureScheme::Secp256k1, SignatureScheme::P256] {
        let mut evm = evm(APPROVE_BOTH, &[]);
        set_placeholders(&mut evm, true);
        let mut tx = payload(vec![frame(FrameMode::Verify, SENDER, 3)], vec![]);
        tx.signatures.push(FrameSignature {
            scheme,
            msg: SignatureMessage::Explicit(B256::with_last_byte(1)),
            ..Default::default()
        });
        assert!(evm.transact(&recovered(tx)).unwrap().discard().status);
    }
}

// A nonempty input delegates approval to BODY, then exits with the requested stop. Empty input
// approves directly, allowing a later frame to retry after an earlier parent failure.
fn delegating_sender(parent_exit: &[u8]) -> Vec<u8> {
    common::delegating_sender(BODY, parent_exit)
}

#[test]
fn nested_approval_commits_only_after_its_parent_succeeds() {
    for keys in [vec![], vec![U256::from(7), U256::MAX]] {
        let mut evm = evm(&delegating_sender(&[0x00]), APPROVE_BOTH);
        let mut delegated = frame(FrameMode::Default, SENDER, 3);
        delegated.data = U256::from(1).to_be_bytes::<32>().to_vec().into();
        let tx = recovered(payload(vec![delegated], keys.clone()));
        let result = evm.transact(&tx).unwrap().commit();
        assert!(result.status);
        let outcome = result.frame.unwrap();
        assert_eq!(outcome.payer, SENDER);
        assert_eq!(outcome.receipts[0].status, FrameStatus::Success);
        assert_eq!(nonce(&mut evm, U256::ZERO), U256::from(u8::from(keys.is_empty())));
        for key in keys {
            assert_eq!(nonce(&mut evm, key), U256::from(1));
        }
    }
}

#[test]
fn reverted_or_halted_parent_discards_nested_approval_and_nonce_debits() {
    for parent_exit in [&[0x5f, 0x5f, 0xfd][..], &[0xfe][..]] {
        for keys in [vec![], vec![U256::from(7), U256::MAX]] {
            let mut evm = evm(&delegating_sender(parent_exit), APPROVE_BOTH);
            let mut delegated = frame(FrameMode::Default, SENDER, 3);
            delegated.data = U256::from(1).to_be_bytes::<32>().to_vec().into();
            let tx = recovered(payload(
                vec![delegated, frame(FrameMode::Verify, SENDER, 3)],
                keys.clone(),
            ));
            let result = evm.transact(&tx).unwrap().commit();
            assert!(result.status);
            let outcome = result.frame.unwrap();
            assert_eq!(outcome.receipts[0].status, FrameStatus::Failure);
            assert_eq!(outcome.receipts[0].gas_used.state, 0);
            assert_eq!(outcome.receipts[1].status, FrameStatus::Success);
            assert_eq!(nonce(&mut evm, U256::ZERO), U256::from(u8::from(keys.is_empty())));
            for key in keys {
                assert_eq!(nonce(&mut evm, key), U256::from(1));
            }
        }
    }
}

#[test]
fn all_key_first_use_charges_are_required_before_any_nonce_lane_advances() {
    let slot_cost = evm2::Version::new(SpecId::BOGOTA).gas_params.get(GasId::SstoreSetState);
    for shortage in [1, slot_cost] {
        let mut evm = evm(APPROVE_BOTH, &[]);
        let keys = vec![U256::from(1), U256::MAX];
        let mut insufficient = frame(FrameMode::Default, SENDER, 3);
        insufficient.limits.state = 2 * slot_cost - shortage;
        let tx = recovered(payload(
            vec![insufficient, frame(FrameMode::Verify, SENDER, 3)],
            keys.clone(),
        ));
        let result = evm.transact(&tx).unwrap().commit();
        let outcome = result.frame.unwrap();
        assert_eq!(outcome.stops[0], InstrStop::OutOfGas);
        assert_eq!(outcome.receipts[0].gas_used.state, 0);
        assert_eq!(outcome.receipts[1].gas_used.state, 2 * slot_cost);
        for key in keys {
            assert_eq!(nonce(&mut evm, key), U256::from(1));
        }
        assert_eq!(nonce(&mut evm, U256::ZERO), U256::ZERO);
    }
}

#[test]
fn mixed_first_use_and_existing_nonce_lanes_charge_only_new_storage() {
    let mut evm = evm(APPROVE_BOTH, &[]);
    let existing = U256::from(7);
    let fresh = U256::MAX;
    evm.overlay_db_mut().insert_account_storage(
        &NONCE_MANAGER,
        &nonce_slot(SENDER, existing),
        &U256::from(3),
    );
    // Sequence checking is optional for simulations, while payment still writes every lane
    // to transaction.sequence + 1 and determines first-use cost from current protocol storage.
    let mut version = *evm.version();
    version.features.remove(EvmFeatures::NONCE_CHECK);
    set_version(&mut evm, version);
    let slot_cost = evm.version().gas_params.get(GasId::SstoreSetState);
    let mut verify = frame(FrameMode::Verify, SENDER, 3);
    verify.limits.state = slot_cost;
    let result =
        evm.transact(&recovered(payload(vec![verify], vec![existing, fresh]))).unwrap().commit();
    assert_eq!(result.frame.unwrap().receipts[0].gas_used.state, slot_cost);
    assert_eq!(nonce(&mut evm, existing), U256::from(1));
    assert_eq!(nonce(&mut evm, fresh), U256::from(1));
}

#[test]
fn mismatched_lane_invalidates_before_any_key_is_consumed() {
    let mut evm = evm(APPROVE_BOTH, &[]);
    let keys = vec![U256::from(7), U256::MAX];
    evm.overlay_db_mut().insert_account_storage(
        &NONCE_MANAGER,
        &nonce_slot(SENDER, keys[1]),
        &U256::from(9),
    );
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER, 3)], keys.clone()));
    assert!(matches!(
        evm.transact(&tx),
        Err(HandlerError::InvalidFrameTransaction(FrameTransactionError::NonceSequenceMismatch))
    ));
    assert_eq!(nonce(&mut evm, keys[0]), U256::ZERO);
    assert_eq!(nonce(&mut evm, keys[1]), U256::from(9));
    assert_eq!(nonce(&mut evm, U256::ZERO), U256::ZERO);
}

#[test]
fn atomic_restore_rollback_restores_the_original_frames_state_gas_ownership() {
    for final_restore in [false, true] {
        let mut evm = evm(APPROVE_BOTH, &[0x5f, 0x35, 0x5f, 0x55, 0x00]);
        let mut write = frame(FrameMode::Sender, BODY, 0);
        write.data = U256::from(1).to_be_bytes::<32>().to_vec().into();
        let mut restore = frame(FrameMode::Sender, BODY, 4);
        restore.limits.state = 0;
        let mut frames = vec![
            frame(FrameMode::Verify, SENDER, 3),
            write,
            restore,
            frame(FrameMode::Sender, FAILURE, 0),
        ];
        if final_restore {
            let mut restore = frame(FrameMode::Sender, BODY, 0);
            restore.limits.state = 0;
            frames.push(restore);
        }
        let result = evm.transact(&recovered(payload(frames, vec![]))).unwrap().commit();
        let outcome = result.frame.unwrap();
        let slot_cost = evm.version().gas_params.get(GasId::SstoreSetState);
        assert_eq!(outcome.receipts[1].gas_used.state, if final_restore { 0 } else { slot_cost });
        assert_eq!(outcome.receipts[2].status, FrameStatus::Success);
        assert!(outcome.rolled_back[2]);
        assert_eq!(outcome.receipts[2].gas_used.state, 0);
        assert_eq!(outcome.receipts[3].status, FrameStatus::Failure);
        assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::from(u8::from(!final_restore)));
    }
}

#[test]
fn later_frame_can_cancel_an_earlier_frames_storage_clear_refund() {
    let mut evm = evm(APPROVE_BOTH, &[0x5f, 0x35, 0x5f, 0x55, 0x00]);
    evm.overlay_db_mut().insert_account_storage(&BODY, &U256::ZERO, &U256::from(7));
    let clear = frame(FrameMode::Sender, BODY, 0);
    let mut rewrite = frame(FrameMode::Sender, BODY, 0);
    rewrite.data = U256::from(9).to_be_bytes::<32>().to_vec().into();
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER, 3), clear, rewrite], vec![]));
    let result = evm.transact(&tx).unwrap().commit();
    assert_eq!(result.refunded, 0);
    assert_eq!(result.state_gas_spent, 0);
    assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::from(9));
}

#[test]
fn a_nested_external_callee_cannot_approve_the_top_level_targets_scope() {
    let mut sender = vec![0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x73];
    sender.extend_from_slice(BODY.as_slice());
    sender.extend_from_slice(&[
        0x5a, 0xf1, // CALL BODY: its APPROVE must fail because its destination is BODY.
        0x5f, 0x52, // Return the child success flag as the direct approval's output.
        0x60, 3, 0x60, 32, 0x5f, 0xaa,
    ]);
    for keys in [vec![], vec![U256::from(7), U256::MAX]] {
        let mut evm = evm(&sender, APPROVE_BOTH);
        let tx = recovered(payload(vec![frame(FrameMode::Default, SENDER, 3)], keys.clone()));
        let result = evm.transact(&tx).unwrap().commit();
        let outcome = result.frame.unwrap();
        assert_eq!(outcome.payer, SENDER);
        assert_eq!(outcome.outputs[0].as_ref(), [0; 32]);
        assert_eq!(nonce(&mut evm, U256::ZERO), U256::from(u8::from(keys.is_empty())));
        for key in keys {
            assert_eq!(nonce(&mut evm, key), U256::from(1));
        }
    }
}

#[test]
fn protocol_nonce_reads_and_writes_do_not_warm_ordinary_evm_accesses() {
    let key = U256::MAX;
    let mut manager = vec![0x7f];
    manager.extend_from_slice(&nonce_slot(SENDER, key).to_be_bytes::<32>());
    manager.extend_from_slice(&[0x54, 0x50, 0x00]); // SLOAD; POP; STOP.
    let mut evm = evm(APPROVE_BOTH, &[]);
    evm.overlay_db_mut().insert_account_info(
        &NONCE_MANAGER,
        AccountInfo::default().with_nonce(1).with_code(Bytecode::new_legacy(manager.into())),
    );
    let tx = recovered(payload(
        vec![
            frame(FrameMode::Verify, SENDER, 3),
            frame(FrameMode::Default, NONCE_MANAGER, 0),
            frame(FrameMode::Default, NONCE_MANAGER, 0),
        ],
        vec![key],
    ));
    let result = evm.transact(&tx).unwrap().commit();
    let outcome = result.frame.unwrap();
    assert_eq!(outcome.receipts[1].gas_used.execution, 3_000 + 3 + 2_100 + 2);
    assert_eq!(outcome.receipts[2].gas_used.execution, 100 + 3 + 100 + 2);
    assert_eq!(nonce(&mut evm, key), U256::from(1));
}

#[test]
fn overlapping_and_disjoint_domains_advance_independently_between_transactions() {
    let mut evm = evm(APPROVE_BOTH, &[]);
    let slot_cost = evm.version().gas_params.get(GasId::SstoreSetState);
    for (keys, sequence, expected_state_gas) in
        [(vec![7, 9], 0, 2 * slot_cost), (vec![9], 1, 0), (vec![11], 0, slot_cost)]
    {
        let mut tx = payload(
            vec![frame(FrameMode::Verify, SENDER, 3)],
            keys.into_iter().map(U256::from).collect(),
        );
        tx.nonce = sequence;
        let result = evm.transact(&recovered(tx)).unwrap().commit();
        assert_eq!(result.frame.unwrap().receipts[0].gas_used.state, expected_state_gas);
    }
    let mut stale_overlap =
        payload(vec![frame(FrameMode::Verify, SENDER, 3)], vec![U256::from(7), U256::from(9)]);
    stale_overlap.nonce = 1;
    assert!(matches!(
        evm.transact(&recovered(stale_overlap)),
        Err(HandlerError::InvalidFrameTransaction(FrameTransactionError::NonceSequenceMismatch))
    ));
    for (key, expected) in [(0, 0), (7, 1), (9, 2), (11, 1)] {
        assert_eq!(nonce(&mut evm, U256::from(key)), U256::from(expected));
    }
}

#[test]
fn keyed_payment_uses_the_active_state_gas_schedule() {
    let mut evm = evm(APPROVE_BOTH, &[]);
    let mut version = *evm.version();
    version.gas_params.set(GasId::SstoreSetState, 17);
    set_version(&mut evm, version);
    let mut verify = frame(FrameMode::Verify, SENDER, 3);
    verify.limits.state = 34;
    let tx = recovered(payload(vec![verify], vec![U256::from(7), U256::MAX]));
    let prefix = evm.validate_frame_prefix(&tx, 1).unwrap();
    assert_eq!(prefix.state_gas, 34);
    assert_eq!(nonce(&mut evm, U256::from(7)), U256::ZERO);
    assert_eq!(nonce(&mut evm, U256::MAX), U256::ZERO);
    let result = evm.transact(&tx).unwrap().commit();
    assert_eq!(result.frame.unwrap().receipts[0].gas_used.state, 34);
    assert_eq!(nonce(&mut evm, U256::from(7)), U256::from(1));
    assert_eq!(nonce(&mut evm, U256::MAX), U256::from(1));
}

mod memory {
    //! Transaction-owner release and safe interpreter reuse after errors and unwinding.

    use super::*;

    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_eips::eip8141::{Frame, FrameMode};
    use alloy_primitives::{Address, Bytes, TxKind, U256};
    use common::{APPROVE_BOTH, BODY, SENDER};
    use core::ops::Deref;
    use evm2::{
        BaseEvmTypes, Evm, Inspector, ethereum::TxEnvelope, evm::AccountInfo,
        interpreter::Interpreter,
    };
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc, Weak,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[derive(Debug)]
    struct OwnedPayload {
        bytes: Vec<u8>,
        _alive: Arc<()>,
    }

    impl AsRef<[u8]> for OwnedPayload {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }

    fn tracked_data() -> (Bytes, Weak<()>) {
        type InnerBytes = <Bytes as Deref>::Target;
        let alive = Arc::new(());
        let weak = Arc::downgrade(&alive);
        let data =
            Bytes(InnerBytes::from_owner(OwnedPayload { bytes: vec![42; 16_384], _alive: alive }));
        (data, weak)
    }

    fn transaction(data: Bytes, allow_approval: bool) -> Recovered<TxEnvelope> {
        let frames = [
            (FrameMode::Verify, SENDER, if allow_approval { 3 } else { 0 }, Bytes::new()),
            (FrameMode::Sender, BODY, 0, data),
        ]
        .into_iter()
        .map(|(mode, target, flags, data)| Frame { flags, data, ..common::frame(mode, target) })
        .collect();
        common::recovered(common::payload(frames))
    }

    fn evm() -> Evm<'static, BaseEvmTypes> {
        common::evm(common::database([
            (SENDER, APPROVE_BOTH),
            (BODY, &[0x60, 42, 0x5f, 0x55, 0x5f, 0x5f, 0xa0, 0x00]),
        ]))
    }

    #[test]
    fn repeated_frame_lifecycles_release_payload_owners_and_bound_cache_entries() {
        let mut evm = evm();
        let valid = transaction(Bytes::new(), true);
        assert!(evm.call_tx(&valid).unwrap().status);
        let expected_cache_sizes = (
            evm.overlay_db().cache.accounts.len(),
            evm.overlay_db().cache.contracts.len(),
            evm.overlay_db().cache.storage.len(),
        );
        for iteration in 0..112 {
            let (data, weak) = tracked_data();
            let tx = transaction(data, iteration % 7 != 6);
            match iteration % 7 {
                0 => {
                    evm.validate_frame_prefix(&tx, 1).unwrap();
                }
                1 => {
                    assert!(evm.validate_frame_prefix(&tx, 2).is_err());
                }
                2 => {
                    drop(evm.transact(&tx).unwrap());
                }
                3 => {
                    assert!(evm.transact(&tx).unwrap().discard().status);
                }
                4 => {
                    drop(evm.transact(&tx).unwrap().detach());
                }
                5 => {
                    assert!(evm.call_tx(&tx).unwrap().status);
                }
                6 => {
                    assert!(evm.transact(&tx).is_err());
                }
                _ => unreachable!(),
            }
            drop(tx);
            assert!(weak.upgrade().is_none(), "payload retained after iteration {iteration}");
            assert!(evm.logs().is_empty());
            // The accepted overlay deliberately caches reads, but repeating the same state must not
            // create a new cache entry, journal, or payload owner for every simulated transaction.
            assert_eq!(
                (
                    evm.overlay_db().cache.accounts.len(),
                    evm.overlay_db().cache.contracts.len(),
                    evm.overlay_db().cache.storage.len(),
                ),
                expected_cache_sizes
            );
            assert!(evm.call_tx(&valid).unwrap().status);
        }
    }

    #[derive(Clone, Copy)]
    enum PanicHook {
        FrameStart,
        Step,
    }

    struct PanicInBody(PanicHook);

    impl Inspector<BaseEvmTypes> for PanicInBody {
        fn frame_start(&mut self, index: usize, _frame: &Frame, _host: &mut Evm<'_, BaseEvmTypes>) {
            if matches!(self.0, PanicHook::FrameStart) && index == 1 {
                panic!("frame entry interrupted");
            }
        }

        fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            if matches!(self.0, PanicHook::Step) && interp.message().destination == BODY {
                panic!("frame interpreter interrupted");
            }
        }
    }

    #[test]
    fn frame_unwind_releases_payload_owners_before_reusing_the_evm() {
        let mut evm = evm();
        for iteration in 0..32 {
            let (data, weak) = tracked_data();
            let tx = transaction(data, true);
            let hook = if iteration % 2 == 0 { PanicHook::FrameStart } else { PanicHook::Step };
            evm.set_inspector(PanicInBody(hook));
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = evm.call_tx(&tx);
                }))
                .is_err()
            );
            let _ = evm.clear_inspector();
            drop(tx);
            assert!(weak.upgrade().is_none(), "payload retained after unwind {iteration}");
            assert!(evm.logs().is_empty());
            assert!(evm.call_tx(&transaction(Bytes::new(), true)).unwrap().status);
        }
    }

    struct PanicOnce {
        panic: bool,
        steps: Arc<AtomicUsize>,
    }

    impl Inspector<BaseEvmTypes> for PanicOnce {
        fn step(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.steps.fetch_add(1, Ordering::Relaxed);
            if core::mem::take(&mut self.panic) {
                panic!("ordinary interpreter interrupted");
            }
        }
    }

    #[test]
    fn ordinary_inspector_unwind_restores_the_frame_pointer_and_discards_scratch() {
        let mut evm = evm();
        let caller = Address::with_last_byte(0xe0);
        evm.overlay_db_mut().insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000)),
        );
        let steps = Arc::new(AtomicUsize::new(0));
        evm.set_inspector(PanicOnce { panic: true, steps: steps.clone() });
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 500_000,
                to: TxKind::Call(BODY),
                ..Default::default()
            }),
            caller,
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = evm.call_tx(&tx);
            }))
            .is_err()
        );
        assert_eq!(steps.load(Ordering::Relaxed), 1);
        assert!(evm.logs().is_empty());
        // The same inspector now runs normally. A dangling current_frame pointer would make the
        // next inspected entry access a freed interpreter; stale scratch would also consume
        // nonce zero.
        assert!(evm.call_tx(&tx).unwrap().status);
        assert!(steps.load(Ordering::Relaxed) > 1);
        assert!(evm.call_tx(&transaction(Bytes::new(), true)).unwrap().status);
    }
}

#[cfg(feature = "async")]
mod async_validation {
    //! Async prefix validation must preserve the frame transaction rollback contract.

    use super::*;

    use alloy_consensus::{TxEip8141, transaction::Recovered};
    use alloy_eips::{
        eip8141::{FrameMode, FrameReceipt, TransactionFees},
        eip8250::{NONCE_MANAGER, nonce_slot},
    };
    use alloy_primitives::{Address, B256, Bytes, U256};
    use common::{APPROVE_BOTH, CountingRunner, SENDER};
    use evm2::{
        BaseEvmTypes, DatabaseError, Evm, Inspector, Precompiles,
        bytecode::Bytecode,
        ethereum::TxEnvelope,
        evm::{
            AccountInfo, DynDatabase, InMemoryDB,
            r#async::{AsyncDatabase, AsyncDb},
        },
        interpreter::InstrStop,
    };
    use std::{
        future::{Future, pending},
        pin::pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Waker},
    };

    const SUSPEND: Address = Address::with_last_byte(0xfe);

    fn database() -> InMemoryDB {
        common::database_with_nonce_manager([(SENDER, APPROVE_BOTH)])
    }

    fn evm() -> Evm<'static, BaseEvmTypes> {
        common::evm(database())
    }

    fn transaction(keyed: bool) -> Recovered<TxEnvelope> {
        common::recovered(TxEip8141 {
            fees: TransactionFees {
                max_fee: U256::from(1_000_000),
                max_priority_fee_per_gas: U256::ONE,
            },
            nonce_keys: Some(if keyed { vec![U256::from(1)] } else { vec![] }),
            ..common::payload(vec![common::frame(FrameMode::Verify, SENDER)])
        })
    }

    fn ready<F: Future>(future: F) -> F::Output {
        match pin!(future).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("synchronous host unexpectedly suspended"),
        }
    }

    const fn assert_send<F: Future + Send>(future: F) -> F {
        future
    }

    #[test]
    fn async_prefix_returns_payment_facts_and_discards_legacy_and_keyed_consumption() {
        for keyed in [false, true] {
            let mut evm = evm();
            let transaction = transaction(keyed);
            let expected = evm.validate_frame_prefix(&transaction, 1).unwrap();
            let actual = ready(evm.validate_frame_prefix_async(&transaction, 1)).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(actual.payer, SENDER);
            assert!(actual.sender_approved);
            assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
            assert_eq!(
                common::stored(&mut evm, NONCE_MANAGER, nonce_slot(SENDER, U256::from(1))),
                U256::ZERO,
            );
            assert!(evm.transact(&transaction).unwrap().discard().status);
        }
    }

    #[test]
    fn send_prefix_future_requires_and_preserves_verified_host_fields() {
        let mut evm = evm();
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        let transaction = transaction(true);
        let validation =
            ready(assert_send(evm.validate_frame_prefix_async_send(&transaction, 1))).unwrap();
        assert_eq!(validation.payer, SENDER);
        assert_eq!(validation.prefix_end, 1);
        assert!(evm.transact(&transaction).unwrap().discard().status);
    }

    struct SuspendAfterApproval(Arc<AtomicUsize>);

    impl Inspector<BaseEvmTypes> for SuspendAfterApproval {
        fn frame_end(
            &mut self,
            _: usize,
            _: &FrameReceipt,
            _: &Bytes,
            _: InstrStop,
            host: &mut Evm<'_, BaseEvmTypes>,
        ) {
            self.0.fetch_add(1, Ordering::Relaxed);
            let _ = host.state_mut().account(&SUSPEND);
        }
    }

    struct SuspendingDatabase(InMemoryDB);

    impl AsyncDatabase for SuspendingDatabase {
        type Error = DatabaseError;

        async fn get_account(
            &mut self,
            address: Address,
        ) -> Result<Option<AccountInfo>, Self::Error> {
            if address == SUSPEND {
                pending::<()>().await;
            }
            DynDatabase::get_account(&mut self.0, &address)
        }

        async fn get_code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
            DynDatabase::get_code_by_hash(&mut self.0, &code_hash)
        }

        async fn get_storage(&mut self, address: Address, key: U256) -> Result<U256, Self::Error> {
            DynDatabase::get_storage(&mut self.0, &address, &key)
        }

        async fn get_block_hash(&mut self, number: U256) -> Result<B256, Self::Error> {
            DynDatabase::get_block_hash(&mut self.0, &number)
        }
    }

    #[test]
    fn cancelling_suspended_prefix_restores_runner_nonce_state_and_execution_guard() {
        for keyed in [false, true] {
            let mut evm = evm();
            evm.set_database(AsyncDb::new(SuspendingDatabase(database())));
            let calls = Arc::new(AtomicUsize::new(0));
            let suspensions = Arc::new(AtomicUsize::new(0));
            evm.set_interpreter_runner(CountingRunner(calls.clone()));
            evm.set_inspector(SuspendAfterApproval(suspensions.clone()));
            let transaction = transaction(keyed);
            {
                let mut validation = pin!(evm.validate_frame_prefix_async(&transaction, 1));
                assert!(
                    validation.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending()
                );
                assert_eq!(suspensions.load(Ordering::Relaxed), 1);
                assert_eq!(calls.load(Ordering::Relaxed), 0);
            }
            evm.clear_inspector();
            assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
            assert_eq!(
                evm.state_mut().account(&SENDER).unwrap().balance(),
                U256::from(1_000_000_000)
            );
            assert!(ready(evm.transact_async(&transaction)).unwrap().discard().status);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn async_prefix_error_does_not_poison_following_full_execution() {
        let mut evm = evm();
        let transaction = transaction(false);
        assert!(ready(evm.validate_frame_prefix_async(&transaction, 0)).is_err());
        assert!(evm.transact(&transaction).unwrap().discard().status);
    }
}
