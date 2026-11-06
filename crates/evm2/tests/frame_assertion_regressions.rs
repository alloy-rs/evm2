//! Bogota assertion selectors, transaction-local views, and ordinary recent-root execution.

mod common;

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_eips::{
    eip8141::{Frame, FrameLimits, FrameMode, FrameStatus},
    eip8272::{RECENT_ROOT_ADDRESS, RECENT_ROOT_CODE},
};
use alloy_primitives::{Address, Bytes, KECCAK256_EMPTY, TxKind, U256, keccak256};
use common::{
    APPROVE_BOTH, BODY, SENDER, install_recent_root, payload, recent_entry, recent_slot,
    recent_tuple, recovered, stored,
};
use evm2::{
    BaseEvmTypes, Evm,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, eip8141::FrameGas},
    evm::AccountInfo,
    interpreter::InstrStop,
};

const OTHER: Address = Address::with_last_byte(0xb1);
const ASSERTION: Address = Address::with_last_byte(0xc0);
const RECEIVER: Address = Address::with_last_byte(0xd0);
const WRITER: Address = Address::with_last_byte(0xe0);
const BALANCE: u64 = 1_000_000_000;

fn frame(mode: FrameMode, target: Address) -> Frame {
    Frame {
        limits: FrameLimits { execution: 200_000, state: 400_000 },
        ..common::frame(mode, target)
    }
}

fn evm(body: &[u8], other: &[u8], assertion: &[u8]) -> Evm<'static, BaseEvmTypes> {
    let mut db = common::database_with_nonce_manager([
        (SENDER, APPROVE_BOTH),
        (BODY, body),
        (OTHER, other),
        (ASSERTION, assertion),
    ]);
    db.insert_account_info(&RECEIVER, AccountInfo::default().with_balance(U256::from(100)));
    db.insert_account_info(&WRITER, AccountInfo::default().with_balance(U256::from(BALANCE)));
    common::evm(db)
}

fn word(address: Address) -> U256 {
    U256::from_be_slice(address.as_slice())
}

fn push(code: &mut Vec<u8>, value: U256) {
    code.push(0x7f);
    code.extend_from_slice(&value.to_be_bytes::<32>());
}

fn push_u64(code: &mut Vec<u8>, value: u64) {
    push(code, U256::from(value));
}

fn store_output(code: &mut Vec<u8>, offset: u64) {
    push_u64(code, offset);
    code.push(0x52);
}

fn trace(code: &mut Vec<u8>, selector: U256, index: U256, offset: u64) {
    push(code, index);
    push(code, selector);
    code.push(0xb6);
    store_output(code, offset);
}

fn return_memory(code: &mut Vec<u8>, offset: u64, len: u64) {
    push_u64(code, len);
    push_u64(code, offset);
    code.push(0xf3);
}

fn words(data: &[u8]) -> Vec<U256> {
    assert_eq!(data.len() % 32, 0);
    data.as_chunks::<32>().0.iter().copied().map(U256::from_be_bytes).collect()
}

#[test]
fn all_assertion_selectors_observe_sorted_net_changes_and_indexed_events() {
    let mut body = vec![0x60, 90, 0x60, 9, 0x55, 0x60, 20, 0x60, 2, 0x55];
    // Deploy a one-byte STOP runtime. The CREATE result is intentionally unused.
    body.extend_from_slice(&[0x63, 0x60, 0x01, 0x5f, 0xf3, 0x5f, 0x52]);
    body.extend_from_slice(&[0x60, 4, 0x60, 28, 0x5f, 0xf0, 0x50]);
    for value in [
        U256::ZERO,
        U256::ZERO,
        U256::ZERO,
        U256::ZERO,
        U256::from(7),
        word(RECEIVER),
        U256::from(30_000),
    ] {
        push(&mut body, value);
    }
    body.extend_from_slice(&[0xf1, 0x50]);
    push_u64(&mut body, 0xabcd);
    store_output(&mut body, 0);
    // Preserve all four topics, including repeated values.
    for value in [0x22, 0x21, 0x21, 0x10, 2, 30] {
        push_u64(&mut body, value);
    }
    body.extend_from_slice(&[0xa4, 0x00]);
    let other = [0x60, 11, 0x60, 1, 0x55, 0x60, 0x21, 0x5f, 0x5f, 0xa1, 0x00];
    let deployed = BODY.create(0);
    let deployed_hash = U256::from_be_bytes(keccak256([0x00]).0);
    let mut nonce_addresses = [SENDER, BODY, deployed];
    nonce_addresses.sort_unstable();
    let mut assertion = Vec::new();
    for selector in 0..=0x1a {
        // Amsterdam's ETH transfer log precedes the explicit LOG4.
        let index = u64::from((0x12..=0x18).contains(&selector));
        trace(&mut assertion, U256::from(selector), U256::from(index), selector * 32);
    }
    let queries = [
        (0x04, 1, word(BODY)),
        (0x05, 1, U256::from(BALANCE)),
        (0x06, 1, U256::from(BALANCE - 7)),
        (0x04, 2, word(RECEIVER)),
        (0x05, 2, U256::from(100)),
        (0x06, 2, U256::from(107)),
        (0x07, 1, word(BODY)),
        (0x0a, 1, U256::from(9)),
        (0x09, 1, U256::from(90)),
        (0x07, 2, word(OTHER)),
        (0x0a, 2, U256::from(1)),
        (0x09, 2, U256::from(11)),
        (0x0e, 1, word(nonce_addresses[1])),
        (0x0f, 1, U256::ZERO),
        (0x10, 1, U256::from(1)),
        (0x0e, 2, word(nonce_addresses[2])),
        (0x0f, 2, U256::ZERO),
        (0x10, 2, U256::from(1)),
        (0x12, 2, word(OTHER)),
        (0x13, 2, U256::from(1)),
        (0x14, 2, U256::from(0x21)),
    ];
    for (i, &(selector, index, _)) in queries.iter().enumerate() {
        trace(&mut assertion, U256::from(selector), U256::from(index), (27 + i as u64) * 32);
    }
    return_memory(&mut assertion, 0, (27 + queries.len() as u64) * 32);
    let mut evm = evm(&body, &other, &assertion);
    let mut tx = payload(vec![
        frame(FrameMode::Verify, SENDER),
        frame(FrameMode::Sender, BODY),
        frame(FrameMode::Sender, OTHER),
        frame(FrameMode::Assert, ASSERTION),
        frame(FrameMode::Assert, ASSERTION),
    ]);
    tx.fees.max_priority_fee_per_gas = U256::from(1);
    let precharge = U256::from(FrameGas::calculate(&tx, evm.version()).unwrap().maximum);
    tx.fees.max_fee = precharge;
    let result = evm.transact(&recovered(tx)).unwrap().commit();
    assert!(result.status, "{:?}", result.frame);
    let result = result.frame.unwrap();
    assert!(result.receipts.iter().all(|r| r.status == FrameStatus::Success));
    let mut expected = vec![
        U256::from(3),
        U256::from(3),
        U256::from(1),
        U256::from(3),
        word(SENDER),
        U256::from(BALANCE),
        U256::from(BALANCE) - precharge,
        word(BODY),
        U256::ZERO,
        U256::from(20),
        U256::from(2),
        word(deployed),
        U256::from_be_bytes(KECCAK256_EMPTY.0),
        deployed_hash,
        word(nonce_addresses[0]),
        U256::ZERO,
        U256::from(1),
        U256::from(3),
        word(BODY),
        U256::from(4),
        U256::from(0x10),
        U256::from(0x21),
        U256::from(0x21),
        U256::from(0x22),
        U256::from(2),
        precharge,
        word(SENDER),
    ];
    expected.extend(queries.iter().map(|q| q.2));
    assert_eq!(words(&result.outputs[3]), expected);
    // A second assertion sees the same immutable in-flight view.
    assert_eq!(result.outputs[3], result.outputs[4]);
    assert_eq!(result.precharge, precharge);
}

#[test]
fn selector_and_index_operands_are_checked_at_full_width() {
    let mut cases = Vec::new();
    for selector in 0..=0x1a {
        let mut code = Vec::new();
        trace(&mut code, U256::from(selector), U256::MAX, 0);
        cases.push(code);
    }
    for selector in [U256::from(0x1b), U256::from(0x100), U256::MAX] {
        let mut code = Vec::new();
        trace(&mut code, selector, U256::ZERO, 0);
        cases.push(code);
    }
    for selector in [0, 1, 2, 3, 0x11, 0x19, 0x1a] {
        let mut code = Vec::new();
        trace(&mut code, U256::from(selector), U256::from(1), 0);
        cases.push(code);
    }
    for (i, code) in cases.iter().enumerate() {
        let mut evm = evm(&[0x60, 42, 0x5f, 0x55, 0x00], &[], code);
        let tx = recovered(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Sender, BODY),
            frame(FrameMode::Assert, ASSERTION),
        ]));
        let result = evm.transact(&tx).unwrap().commit();
        assert!(!result.status, "case {i}");
        let result = result.frame.unwrap();
        assert_eq!(result.stops[2], InstrStop::OpcodeNotFound, "case {i}");
        assert_eq!(result.assertion_failure, Some(2));
        assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
        assert_eq!(stored(&mut evm, BODY, U256::ZERO), U256::ZERO);
    }
}

#[test]
fn event_copy_bounds_and_zero_length_follow_return_data_rules() {
    let body = [0x60, 0xab, 0x5f, 0x53, 0x60, 1, 0x5f, 0xa0, 0x00];
    for (index, memory, offset, len, stop, expected) in [
        (U256::ZERO, U256::ZERO, U256::ZERO, 1, InstrStop::Return, vec![0xab]),
        (U256::ZERO, U256::MAX, U256::from(1), 0, InstrStop::Return, vec![]),
        (U256::ZERO, U256::ZERO, U256::from(1), 1, InstrStop::OutOfOffset, vec![]),
        (U256::ZERO, U256::ZERO, U256::MAX, 0, InstrStop::OutOfOffset, vec![]),
        (U256::from(1), U256::ZERO, U256::ZERO, 0, InstrStop::OpcodeNotFound, vec![]),
        (U256::MAX, U256::ZERO, U256::ZERO, 0, InstrStop::OpcodeNotFound, vec![]),
    ] {
        let mut assertion = Vec::new();
        for value in [U256::from(len), offset, memory, index] {
            push(&mut assertion, value);
        }
        assertion.push(0xb7);
        return_memory(&mut assertion, 0, len);
        let mut evm = evm(&body, &[], &assertion);
        let tx = recovered(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Sender, BODY),
            frame(FrameMode::Assert, ASSERTION),
        ]));
        let result = evm.transact(&tx).unwrap().discard().frame.unwrap();
        assert_eq!(result.stops[2], stop, "index {index}, offset {offset}, len {len}");
        assert_eq!(result.outputs[2].as_ref(), expected);
    }
}

#[test]
fn reverted_logs_and_restored_slots_are_absent_and_views_reset_between_transactions() {
    // The successful body changes a slot back to its transaction original.
    let body = [0x60, 1, 0x5f, 0x55, 0x5f, 0x5f, 0x55, 0x5f, 0x5f, 0xa0, 0x00];
    let reverted = [0x60, 2, 0x5f, 0x55, 0x5f, 0x5f, 0xa0, 0x5f, 0x5f, 0xfd];
    let mut assertion = Vec::new();
    trace(&mut assertion, U256::from(1), U256::ZERO, 0);
    trace(&mut assertion, U256::from(0x11), U256::ZERO, 32);
    return_memory(&mut assertion, 0, 64);
    let mut evm = evm(&body, &reverted, &assertion);
    let first = recovered(payload(vec![
        frame(FrameMode::Verify, SENDER),
        frame(FrameMode::Sender, BODY),
        frame(FrameMode::Sender, OTHER),
        frame(FrameMode::Assert, ASSERTION),
    ]));
    let result = evm.transact(&first).unwrap().commit().frame.unwrap();
    assert_eq!(result.receipts[2].status, FrameStatus::Failure);
    assert_eq!(words(&result.outputs[3]), [U256::ZERO, U256::from(1)]);
    let mut second =
        payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Assert, ASSERTION)]);
    second.nonce = 1;
    let result = evm.transact(&recovered(second)).unwrap().commit().frame.unwrap();
    assert_eq!(words(&result.outputs[1]), [U256::ZERO; 2]);
}

#[test]
fn assertion_opcodes_are_unavailable_to_body_and_ordinary_transactions() {
    for opcode in [0xb6, 0xb7, 0xb8] {
        let code = [0x5f, 0x5f, 0x5f, 0x5f, opcode, 0x00];
        let mut evm = evm(&code, &[], &[]);
        let tx = recovered(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Sender, BODY),
        ]));
        let result = evm.transact(&tx).unwrap().discard().frame.unwrap();
        assert_eq!(result.receipts[1].status, FrameStatus::Failure);
        assert_eq!(result.stops[1], InstrStop::OpcodeNotFound);

        let ordinary = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 1_000_000,
                to: TxKind::Call(BODY),
                ..Default::default()
            }),
            WRITER,
        );
        let result = evm.transact(&ordinary).unwrap().discard();
        assert!(!result.status);
        assert_eq!(result.stop, InstrStop::OpcodeNotFound);
    }
}

#[test]
fn keyed_nonce_approval_does_not_invent_an_account_nonce_change() {
    let mut assertion = Vec::new();
    trace(&mut assertion, U256::from(3), U256::ZERO, 0);
    trace(&mut assertion, U256::from(1), U256::ZERO, 32);
    return_memory(&mut assertion, 0, 64);
    let mut evm = evm(&[], &[], &assertion);
    let mut tx =
        payload(vec![frame(FrameMode::Verify, SENDER), frame(FrameMode::Assert, ASSERTION)]);
    tx.nonce_keys = Some(vec![U256::from(7)]);
    let result = evm.transact(&recovered(tx)).unwrap().commit();
    assert!(result.status, "{:?}", result.frame);
    assert_eq!(words(&result.frame.unwrap().outputs[1]), [U256::ZERO, U256::from(1)]);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
}

#[test]
fn old_event_copy_byte_is_undefined_and_repurposed_byte_requires_four_operands() {
    for (code, expected) in [
        (vec![0x5f, 0x5f, 0x5f, 0x5f, 0xb8], InstrStop::OpcodeNotFound),
        (vec![0x5f, 0x5f, 0x5f, 0xb7], InstrStop::StackUnderflow),
    ] {
        let mut evm = evm(&[], &[], &code);
        let tx = recovered(payload(vec![
            frame(FrameMode::Verify, SENDER),
            frame(FrameMode::Assert, ASSERTION),
        ]));
        let result = evm.transact(&tx).unwrap().discard();
        assert!(!result.status);
        assert_eq!(result.frame.unwrap().stops[1], expected);
    }
}

fn recent_query(target: Address, data: Vec<u8>) -> Recovered<TxEnvelope> {
    let mut verifier = frame(FrameMode::Verify, target);
    verifier.flags = 0;
    verifier.data = data.into();
    recovered(payload(vec![verifier, frame(FrameMode::Verify, SENDER)]))
}

#[test]
fn recent_root_write_read_and_ring_overwrite_execute_ordinary_contract_code() {
    let mut evm = evm(&[], &[], &[]);
    install_recent_root(&mut evm);
    let salt = [0x11; 32];
    let mut source_input = WRITER.as_slice().to_vec();
    source_input.extend_from_slice(&salt);
    let source = keccak256(source_input).0;
    for (nonce, slot, root) in [(0, 100, [0x22; 32]), (1, 8292, [0x33; 32])] {
        evm.set_block(BlockEnvExt {
            slot_num: U256::from(slot),
            timestamp: U256::from(987_654),
            ..*evm.block()
        });
        let mut data = salt.to_vec();
        data.extend_from_slice(&root);
        let write = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                nonce,
                gas_limit: 1_000_000,
                to: TxKind::Call(RECENT_ROOT_ADDRESS),
                input: data.into(),
                ..Default::default()
            }),
            WRITER,
        );
        assert!(evm.transact(&write).unwrap().commit().status);
        let tuple = recent_tuple(source, slot, root);
        assert_eq!(
            stored(&mut evm, RECENT_ROOT_ADDRESS, recent_slot(source, slot)),
            recent_entry(&tuple)
        );
        assert!(evm.transact(&recent_query(RECENT_ROOT_ADDRESS, tuple.clone())).is_err());
        evm.set_block(BlockEnvExt { slot_num: U256::from(slot + 1), ..*evm.block() });
        assert!(evm.transact(&recent_query(RECENT_ROOT_ADDRESS, tuple)).unwrap().discard().status);
        if nonce == 1 {
            // Even with the new slot, an overwritten root does not satisfy the commitment.
            assert!(
                evm.transact(&recent_query(
                    RECENT_ROOT_ADDRESS,
                    recent_tuple(source, slot, [0x22; 32])
                ))
                .is_err()
            );
            assert!(
                evm.transact(&recent_query(
                    RECENT_ROOT_ADDRESS,
                    recent_tuple(source, 100, [0x22; 32])
                ))
                .is_err()
            );
        }
    }
}

#[test]
fn recent_root_verification_checks_every_tuple_and_delegated_storage_context() {
    let source = [0x11; 32];
    let tuple = recent_tuple(source, 100, [0x22; 32]);
    let alias = Address::with_last_byte(0xf0);
    let mut evm = evm(&[], &[], &[]);
    install_recent_root(&mut evm);
    evm.overlay_db_mut().insert_account_info(
        &alias,
        AccountInfo::default().with_code(Bytecode::new_eip7702(RECENT_ROOT_ADDRESS)),
    );
    evm.overlay_db_mut().insert_account_storage(
        &alias,
        &recent_slot(source, 100),
        &recent_entry(&tuple),
    );
    evm.set_block(BlockEnvExt { slot_num: U256::from(101), ..*evm.block() });
    // Delegated execution reads the alias's state, not the implementation account's state.
    assert!(evm.transact(&recent_query(alias, tuple.clone())).unwrap().discard().status);
    assert!(evm.transact(&recent_query(RECENT_ROOT_ADDRESS, tuple.clone())).is_err());
    assert!(evm.transact(&recent_query(alias, tuple.repeat(16))).unwrap().discard().status);
    let mut changed_source = tuple.clone();
    changed_source[0] ^= 1;
    let mut changed_slot = tuple.clone();
    changed_slot[39] ^= 1;
    let mut changed_root = tuple.clone();
    changed_root[71] ^= 1;
    let mut truncated = tuple.clone();
    truncated.pop();
    let mut trailing = tuple.clone();
    trailing.push(0);
    for invalid in [
        vec![],
        tuple.repeat(17),
        changed_source,
        changed_slot,
        changed_root.clone(),
        truncated,
        trailing,
    ] {
        assert!(evm.transact(&recent_query(alias, invalid)).is_err());
    }
    let mut second_invalid = tuple.clone();
    second_invalid.extend_from_slice(&changed_root);
    assert!(evm.transact(&recent_query(alias, second_invalid)).is_err());
    // Failure cleanup does not poison a later valid executable-prefix check.
    let valid = recent_query(alias, tuple);
    let prefix = evm.validate_frame_prefix(&valid, 2).unwrap();
    assert_eq!(prefix.payer, SENDER);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
}

#[test]
fn recent_root_address_has_no_implicit_precompile_or_installed_runtime() {
    for (runtime, valid) in [
        (None, false),
        (Some(RECENT_ROOT_CODE.clone()), false),
        (Some(Bytes::from_static(&[0x00])), true),
    ] {
        let mut evm = evm(&[], &[], &[]);
        if let Some(runtime) = runtime {
            evm.overlay_db_mut().insert_account_info(
                &RECENT_ROOT_ADDRESS,
                AccountInfo::default().with_code(Bytecode::new_legacy(runtime)),
            );
        }
        // Malformed verifier data is governed entirely by the actual deployed runtime.
        assert_eq!(
            evm.transact(&recent_query(RECENT_ROOT_ADDRESS, vec![1]))
                .map(|tx| tx.discard())
                .is_ok(),
            valid
        );
    }
}

#[test]
fn recent_root_write_is_rejected_in_static_assertions_and_preserves_the_payment_prefix() {
    let mut evm = evm(&[], &[], &[]);
    install_recent_root(&mut evm);
    evm.set_block(BlockEnvExt { slot_num: U256::from(100), ..*evm.block() });
    let mut write = frame(FrameMode::Assert, RECENT_ROOT_ADDRESS);
    write.data = vec![0x11; 64].into();
    let tx = recovered(payload(vec![frame(FrameMode::Verify, SENDER), write]));
    let result = evm.transact(&tx).unwrap().commit();
    assert!(!result.status);
    let result = result.frame.unwrap();
    assert_eq!(result.stops[1], InstrStop::StateChangeDuringStaticCall);
    assert_eq!(result.assertion_failure, Some(1));
    assert_eq!(result.receipts[1].gas_used.state, 0);
    assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
}
