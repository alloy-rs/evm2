//! Frame execution matrices, generated rollback sequences, and custom host integration.

mod common;

use alloy_consensus::TxEip8141;
use alloy_eips::{
    eip8141::{Frame, FrameMode, FrameReceipt, FrameStatus},
    eip8250::{NONCE_MANAGER, nonce_calldata, nonce_slot},
};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use common::{APPROVE_BOTH, BODY, SENDER, frame};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, Version,
    constants::EIP7708_TRANSFER_TOPIC,
    env::BlockEnvExt,
    ethereum::{eip8141::FrameGas, ethereum_tx_registry},
    evm::SYSTEM_ADDRESS,
    version::GasId,
};
use std::collections::BTreeMap;

const OTHER: Address = Address::with_last_byte(0xc0);

#[derive(Clone)]
struct Case {
    name: String,
    tx: TxEip8141,
    body: Bytes,
    other: Bytes,
    sender_code: Bytes,
    storage: Vec<(Address, U256, U256)>,
    basefee: u64,
    expected_valid: bool,
    expected_success: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Account {
    balance: U256,
    nonce: u64,
    code_hash: B256,
    storage: BTreeMap<U256, U256>,
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    success: bool,
    spent: u64,
    state_gas: u64,
    refunded: u64,
    floor: u64,
    fee_gas: u64,
    block_execution: u64,
    payer: Address,
    receipts: Vec<FrameReceipt>,
    outputs: Vec<Bytes>,
    logs: Vec<Log>,
    accounts: BTreeMap<Address, Account>,
}

fn base_case(name: &str, body: &[u8], other: &[u8], suffix: Vec<Frame>) -> Case {
    let mut frames = vec![frame(FrameMode::Verify, SENDER)];
    frames.extend(suffix);
    Case {
        name: name.into(),
        tx: common::payload(frames),
        body: Bytes::copy_from_slice(body),
        other: Bytes::copy_from_slice(other),
        sender_code: Bytes::from_static(APPROVE_BOTH),
        storage: Vec::new(),
        basefee: 0,
        expected_valid: true,
        expected_success: None,
    }
}

fn initial(case: &Case) -> Vec<(Address, Bytes)> {
    vec![(SENDER, case.sender_code.clone()), (BODY, case.body.clone()), (OTHER, case.other.clone())]
}

fn prestate(case: &Case) -> BTreeMap<Address, Account> {
    let mut accounts = initial(case)
        .into_iter()
        .map(|(address, code)| {
            (
                address,
                Account {
                    balance: U256::from(1_000_000_000),
                    code_hash: alloy_primitives::keccak256(code),
                    ..Default::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    accounts.insert(
        NONCE_MANAGER,
        Account { nonce: 1, code_hash: alloy_primitives::KECCAK256_EMPTY, ..Default::default() },
    );
    for &(address, key, value) in &case.storage {
        accounts.get_mut(&address).unwrap().storage.insert(key, value);
    }
    accounts
}

fn native(case: &Case) -> Result<Outcome, String> {
    let mut db = common::database_with_nonce_manager([
        (SENDER, case.sender_code.as_ref()),
        (BODY, case.body.as_ref()),
        (OTHER, case.other.as_ref()),
    ]);
    for &(address, key, value) in &case.storage {
        db.insert_account_storage(&address, &key, &value);
    }
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::BOGOTA,
        BlockEnvExt { basefee: U256::from(case.basefee), ..Default::default() },
        ethereum_tx_registry(SpecId::BOGOTA),
        db,
        Precompiles::base(SpecId::BOGOTA),
    );
    let tx = common::recovered(case.tx.clone());
    let executed = evm.transact(&tx).map_err(|error| format!("{error:?}"))?;
    let mut accounts = prestate(case);
    for change in executed.account_changes() {
        if let Some(info) = change.current.filter(|info| !info.is_empty()) {
            let entry = accounts.entry(change.address).or_default();
            entry.balance = info.balance;
            entry.nonce = info.nonce;
            entry.code_hash = info.code_hash;
            for (key, slot) in &change.storage.slots {
                if slot.value.current.is_zero() {
                    entry.storage.remove(key);
                } else {
                    entry.storage.insert(*key, slot.value.current);
                }
            }
        } else {
            accounts.remove(&change.address);
        }
    }
    let result = executed.discard();
    let frame = result.frame.as_ref().unwrap();
    Ok(Outcome {
        success: result.status,
        spent: result.total_gas_spent,
        state_gas: result.state_gas_spent,
        refunded: result.refunded,
        floor: result.floor_gas,
        fee_gas: result.tx_gas_used(),
        block_execution: result.execution_gas_spent(),
        payer: frame.payer,
        receipts: frame.receipts.clone(),
        outputs: frame.outputs.clone(),
        logs: result.logs,
        accounts,
    })
}

fn cases() -> Vec<Case> {
    let mut cases = vec![
        base_case("payment", &[], &[], vec![]),
        base_case(
            "sstore",
            &[0x60, 42, 0x5f, 0x55, 0x00],
            &[],
            vec![frame(FrameMode::Sender, BODY)],
        ),
        base_case(
            "assertion_revert",
            &[0x60, 42, 0x5f, 0x55, 0x00],
            &[0x5f, 0x5f, 0xfd],
            vec![
                frame(FrameMode::Sender, BODY),
                frame(FrameMode::Assert, OTHER),
                frame(FrameMode::Assert, OTHER),
            ],
        ),
        base_case(
            "assertion_trace",
            &[0x60, 42, 0x5f, 0x55, 0x00],
            &[0x5f, 0x60, 1, 0xb6, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3],
            vec![frame(FrameMode::Sender, BODY), frame(FrameMode::Assert, OTHER)],
        ),
    ];
    for (keys, valid) in [
        (vec![], true),
        (vec![0], true),
        (vec![1], true),
        (vec![1, 2], true),
        (vec![0, 1], true),
        (vec![0, 0], false),
        (vec![2, 1], false),
        ((0..17).collect(), false),
    ] {
        let mut case = base_case(&format!("nonce_keys_{keys:?}"), &[], &[], vec![]);
        case.expected_valid = valid;
        case.tx.nonce_keys = Some(keys.into_iter().map(U256::from).collect());
        cases.push(case);
    }
    let mut legacy_max = base_case("legacy_max_nonce_is_invalid", &[], &[], vec![]);
    legacy_max.tx.nonce = u64::MAX;
    legacy_max.expected_valid = false;
    cases.push(legacy_max);
    for keys in [vec![U256::ZERO], vec![U256::from(1)], vec![U256::ZERO, U256::from(1)]] {
        for current in [U256::ZERO, U256::from(1), U256::from(u64::MAX), U256::from(1) << 64] {
            let mut case =
                base_case(&format!("single_use_keys_{keys:?}_current_{current}"), &[], &[], vec![]);
            case.tx.nonce_keys = Some(keys.clone());
            case.tx.nonce = u64::MAX;
            case.expected_valid = current.is_zero();
            case.storage.push((NONCE_MANAGER, nonce_slot(SENDER, *keys.last().unwrap()), current));
            cases.push(case);
        }
    }
    let mut sequential_max = base_case("sequential_key_exhaustion", &[], &[], vec![]);
    sequential_max.tx.nonce_keys = Some(vec![U256::ZERO]);
    sequential_max.tx.nonce = u64::MAX - 1;
    sequential_max.storage.push((
        NONCE_MANAGER,
        nonce_slot(SENDER, U256::ZERO),
        U256::from(u64::MAX - 1),
    ));
    cases.push(sequential_max);
    for state in [0, 1, 97_919, 97_920, 300_000] {
        let mut f = frame(FrameMode::Sender, BODY);
        f.limits.state = state;
        cases.push(base_case(
            &format!("sstore_state_{state}"),
            &[0x60, 1, 0x5f, 0x55, 0x00],
            &[],
            vec![f],
        ));
    }
    for execution in [0, 99, 100, 2_999, 3_000, 100_000] {
        let mut f = frame(FrameMode::Sender, BODY);
        f.limits.execution = execution;
        cases.push(base_case(&format!("entry_execution_{execution}"), &[0x00], &[], vec![f]));
    }
    for selector in 0..=0x1bu8 {
        let query = [0x5f, 0x60, selector, 0xb6, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3];
        cases.push(base_case(
            &format!("txtrace_{selector:02x}"),
            &[
                0x60, 42, 0x5f, 0x55, 0x60, 0xab, 0x5f, 0x52, 0x60, 7, 0x60, 9, 0x60, 32, 0x5f,
                0xa2, 0x00,
            ],
            &query,
            vec![frame(FrameMode::Sender, BODY), frame(FrameMode::Assert, OTHER)],
        ));
    }
    for pattern in 0..32u32 {
        let suffix = (0..5)
            .map(|index| {
                let mut f = frame(FrameMode::Sender, BODY);
                let value = U256::from((pattern >> index) & 1);
                f.data = value.to_be_bytes::<32>().to_vec().into();
                f.limits.state = if index == 0 { 97_920 } else { 0 };
                f
            })
            .collect();
        cases.push(base_case(
            &format!("storage_ownership_{pattern:02x}"),
            &[0x5f, 0x35, 0x5f, 0x55, 0x00],
            &[],
            suffix,
        ));
    }
    cases.extend(additional_cases());
    cases.extend(assertion_cases());
    cases
}

fn additional_cases() -> Vec<Case> {
    let store = [0x5f, 0x35, 0x5f, 0x55, 0x00];
    let revert = [0x5f, 0x5f, 0xfd];
    let mut cases = Vec::new();
    let version = Version::base(SpecId::BOGOTA);
    for (budget_per_max_gas, priority, basefee) in
        [(1, 1, 0), (9, 2, 3), (10, 0, 3), (3, 3, 3), (4, 3, 3)]
    {
        for assertion_fails in [false, true] {
            let mut suffix = vec![frame(FrameMode::Sender, BODY)];
            if assertion_fails {
                suffix.push(frame(FrameMode::Assert, OTHER));
            }
            let mut case = base_case(
                &format!(
                    "paid_budget_{budget_per_max_gas}_priority_{priority}_base_{basefee}_assert_{assertion_fails}"
                ),
                &[0x60, 42, 0x5f, 0x55, 0x00],
                &revert,
                suffix,
            );
            case.basefee = basefee;
            case.tx.fees.max_fee =
                U256::from(FrameGas::calculate(&case.tx, version).unwrap().maximum)
                    * U256::from(budget_per_max_gas);
            case.tx.fees.max_priority_fee_per_gas = U256::from(priority);
            cases.push(case);
        }
    }
    for values in [vec![0], vec![0, 7], vec![0, 1], vec![0, 7, 0], vec![1, 0, 7], vec![1, 0]] {
        let suffix = values
            .iter()
            .map(|value| {
                let mut f = frame(FrameMode::Sender, BODY);
                f.data = U256::from(*value).to_be_bytes::<32>().to_vec().into();
                f.limits.state = 0;
                f
            })
            .collect();
        let mut case =
            base_case(&format!("nonzero_original_refund_{values:?}"), &store, &[], suffix);
        case.storage.push((BODY, U256::ZERO, U256::from(7)));
        case.tx.fees.max_fee =
            U256::from(FrameGas::calculate(&case.tx, version).unwrap().maximum) * U256::from(3);
        case.tx.fees.max_priority_fee_per_gas = U256::from(1);
        cases.push(case);
    }
    for assertion_fails in [false, true] {
        let mut payer = frame(FrameMode::Verify, OTHER);
        payer.flags = 1;
        let mut suffix = vec![payer, frame(FrameMode::Sender, BODY)];
        if assertion_fails {
            // APPROVE is forbidden in ASSERT, so the payer contract also supplies
            // a deterministic failing assertion after its successful payment prefix.
            suffix.push(frame(FrameMode::Assert, OTHER));
        }
        let mut case = base_case(
            &format!("external_payer_assert_{assertion_fails}"),
            &[0x60, 42, 0x5f, 0x55, 0x00],
            &[0x60, 1, 0x5f, 0x5f, 0xaa],
            suffix,
        );
        case.sender_code = Bytes::from_static(&[0x60, 2, 0x5f, 0x5f, 0xaa]);
        case.tx.fees.max_fee =
            U256::from(FrameGas::calculate(&case.tx, version).unwrap().maximum) * U256::from(9);
        case.tx.fees.max_priority_fee_per_gas = U256::from(2);
        case.basefee = 3;
        cases.push(case);
    }
    let mut first = frame(FrameMode::Sender, BODY);
    first.flags = 4;
    let mut failing = frame(FrameMode::Sender, OTHER);
    failing.flags = 4;
    cases.push(base_case(
        "atomic_storage_rollback_and_skip",
        &[0x60, 42, 0x5f, 0x55, 0x00],
        &revert,
        vec![first.clone(), failing, frame(FrameMode::Sender, BODY)],
    ));
    let mut case = base_case(
        "atomic_nonzero_original_refund_rollback",
        &[0x5f, 0x5f, 0x55, 0x00],
        &revert,
        vec![first, frame(FrameMode::Sender, OTHER)],
    );
    case.storage.push((BODY, U256::ZERO, U256::from(7)));
    cases.push(case);
    cases.push(base_case(
        "created_account_and_code_stream",
        &[
            0x63, 0x60, 0x01, 0x5f, 0xf3, 0x5f, 0x52, 0x60, 4, 0x60, 28, 0x5f, 0xf0, 0x5f, 0x52,
            0x60, 32, 0x5f, 0xf3,
        ],
        &[],
        vec![frame(FrameMode::Sender, BODY)],
    ));
    let mut sendall = vec![0x7f];
    sendall.extend_from_slice(
        &U256::from_be_slice(&[0x60, 2, 0x60, 10, 0x5f, 0x39, 0x60, 2, 0x5f, 0xf3, 0x5f, 0xff])
            .to_be_bytes::<32>(),
    );
    sendall.extend_from_slice(&[
        0x5f, 0x52, 0x60, 12, 0x60, 20, 0x5f, 0xf0, 0x5f, 0x52, 0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x5f,
        0x51, 0x61, 0xc3, 0x50, 0xf1, 0x50, 0x00,
    ]);
    cases.push(base_case(
        "created_then_sendall_stream",
        &sendall,
        &[],
        vec![frame(FrameMode::Sender, BODY)],
    ));
    cases
}

fn assertion_cases() -> Vec<Case> {
    // Deploy nonempty stored code, change creator/created account nonces and storage,
    // and emit one event with four topics and a 32-byte data payload ending in 0xab.
    let body = [
        0x63, 0x60, 0x01, 0x5f, 0xf3, 0x5f, 0x52, 0x60, 4, 0x60, 28, 0x5f, 0xf0, 0x50, 0x60, 42,
        0x5f, 0x55, 0x60, 0xab, 0x5f, 0x52, 0x60, 0x44, 0x60, 0x33, 0x60, 0x22, 0x60, 0x11, 0x60,
        32, 0x5f, 0xa4, 0x00,
    ];
    let mut cases = Vec::new();
    for keyed in [false, true] {
        for selector in 0..=0x1au8 {
            let event_index = u8::from((0x12..=0x18).contains(&selector));
            let query = [0x60, event_index, 0x60, selector, 0xb6, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3];
            let mut case = assertion_case(
                &format!("txtrace_net_changes_{selector:02x}_keyed_{keyed}"),
                &body,
                &query,
                true,
            );
            case.tx.frames[1].value = U256::from(7);
            if keyed {
                case.tx.nonce_keys = Some(vec![U256::ZERO]);
            }
            cases.push(case);
        }
    }
    for selector in [0, 1, 2, 3, 0x11, 0x19, 0x1a] {
        let query = [0x60, 1, 0x60, selector, 0xb6, 0x00];
        cases.push(assertion_case(
            &format!("txtrace_scalar_rejects_nonzero_index_{selector:02x}"),
            &body,
            &query,
            false,
        ));
    }
    for (index, memory, offset, len, success) in [
        (U256::ZERO, U256::ZERO, U256::ZERO, 32, true),
        (U256::ZERO, U256::ZERO, U256::from(31), 4, false),
        (U256::ZERO, U256::from(1), U256::ZERO, 33, false),
        (U256::ZERO, U256::from(1), U256::ZERO, 32, true),
        (U256::ZERO, U256::MAX, U256::from(32), 0, true),
        (U256::ZERO, U256::MAX, U256::MAX, 0, false),
        (U256::from(1), U256::ZERO, U256::ZERO, 32, false),
        (U256::MAX, U256::ZERO, U256::ZERO, 0, false),
    ] {
        let mut query = Vec::new();
        for operand in [U256::from(len), offset, memory, index] {
            query.push(0x7f);
            query.extend_from_slice(&operand.to_be_bytes::<32>());
        }
        query.extend_from_slice(&[0xb7, 0x60, 64, 0x5f, 0xf3]);
        cases.push(assertion_case(
            &format!("eventdatacopy_{index}_{memory}_{offset}_{len}"),
            &body,
            &query,
            success,
        ));
    }
    cases.push(assertion_case(
        "old_eventdatacopy_byte_is_undefined",
        &body,
        &[0x5f, 0x5f, 0x5f, 0x5f, 0xb8, 0x00],
        false,
    ));
    cases
}

fn word(value: U256) -> Bytes {
    Bytes::copy_from_slice(&value.to_be_bytes::<32>())
}

fn address_word(address: Address) -> U256 {
    U256::from_be_slice(address.as_slice())
}

fn put_storage(
    accounts: &mut BTreeMap<Address, Account>,
    address: Address,
    key: U256,
    value: U256,
) {
    let storage = &mut accounts.get_mut(&address).unwrap().storage;
    if value.is_zero() {
        storage.remove(&key);
    } else {
        storage.insert(key, value);
    }
}

fn assert_native_case(case: &Case, result: &Outcome) {
    let version = Version::base(SpecId::BOGOTA);
    let gp = &version.gas_params;
    let slot_cost = gp.get(GasId::SstoreSetState);
    let create_cost = gp.create_state_gas() + gp.code_deposit_state_gas(1);
    let mut statuses = vec![FrameStatus::Success; case.tx.frames.len()];
    let mut executions = vec![107; case.tx.frames.len()];
    let mut state = vec![0; case.tx.frames.len()];
    let mut outputs = vec![Bytes::new(); case.tx.frames.len()];
    let mut receipt_logs = vec![Vec::new(); case.tx.frames.len()];
    let mut accounts = prestate(case);
    let keys = case.tx.nonce_keys.as_deref().unwrap_or_default();
    if keys.is_empty() {
        accounts.get_mut(&SENDER).unwrap().nonce = 1;
    } else {
        for &key in keys {
            let slot = nonce_slot(SENDER, key);
            if accounts[&NONCE_MANAGER].storage.get(&slot).is_none_or(U256::is_zero) {
                state[0] += slot_cost;
            }
            put_storage(
                &mut accounts,
                NONCE_MANAGER,
                slot,
                U256::from(case.tx.nonce.saturating_add(1)),
            );
        }
    }
    let name = case.name.as_str();
    let mut success = true;
    let mut raw_refund = 0;
    let payer = if name.starts_with("external_payer_") { OTHER } else { SENDER };
    if name.starts_with("external_payer_") {
        executions[1] = 3_007;
        executions[2] = 15_105;
        state[2] = slot_cost;
        if name.ends_with("true") {
            success = false;
            statuses[3] = FrameStatus::Failure;
            executions[3] = case.tx.frames[3].limits.execution;
            state[2] = 0;
        } else {
            put_storage(&mut accounts, BODY, U256::ZERO, U256::from(42));
        }
    } else if name.starts_with("storage_ownership_") {
        // Only the first frame has a slot-creation reservoir. A later attempt to
        // recreate a cleared slot fails its independent state-gas budget.
        let first = !U256::from_be_slice(&case.tx.frames[1].data).is_zero();
        let mut present = first;
        executions[1] = if first { 15_107 } else { 5_107 };
        for index in 2..case.tx.frames.len() {
            let next = !U256::from_be_slice(&case.tx.frames[index].data).is_zero();
            if next && !present {
                statuses[index] = FrameStatus::Failure;
                executions[index] = 100_000;
            } else {
                executions[index] = 207;
                if present && !next {
                    raw_refund += gp.get(GasId::SstoreSetRefund);
                }
                present = next;
            }
        }
        state[1] = if present { slot_cost } else { 0 };
        put_storage(&mut accounts, BODY, U256::ZERO, U256::from(u8::from(present)));
    } else if name.starts_with("nonzero_original_refund_") {
        let mut present = U256::from(7);
        for index in 1..case.tx.frames.len() {
            let next = U256::from_be_slice(&case.tx.frames[index].data);
            executions[index] = if index == 1 { 5_107 } else { 207 };
            if present == U256::from(7) && next != present {
                executions[index] += 10_000;
            }
            if next == U256::from(7) && next != present {
                raw_refund += gp.get(GasId::SstoreResetRefund);
            }
            present = next;
        }
        if present.is_zero() {
            raw_refund += gp.get(GasId::SstoreClearingSlotRefund);
        }
        put_storage(&mut accounts, BODY, U256::ZERO, present);
    } else if name.starts_with("entry_execution_") {
        let limit = case.tx.frames[1].limits.execution;
        executions[1] = limit.min(3_000);
        if limit < 3_000 {
            statuses[1] = FrameStatus::Failure;
        }
    } else if name.starts_with("sstore_state_") {
        let reservoir = case.tx.frames[1].limits.state;
        if reservoir < slot_cost {
            statuses[1] = FrameStatus::Failure;
            executions[1] = 100_000;
        } else {
            executions[1] = 15_105;
            state[1] = slot_cost;
            put_storage(&mut accounts, BODY, U256::ZERO, U256::from(1));
        }
    } else if name == "atomic_storage_rollback_and_skip" {
        executions[1..].copy_from_slice(&[15_105, 3_004, 0]);
        statuses[2] = FrameStatus::Failure;
        statuses[3] = FrameStatus::SkippedAtomicBatch;
    } else if name == "atomic_nonzero_original_refund_rollback" {
        executions[1..].copy_from_slice(&[15_104, 3_004]);
        statuses[2] = FrameStatus::Failure;
    } else if name == "created_account_and_code_stream" {
        // CREATE access + one initcode word, the eight-gas initcode, memory,
        // pushes and the EIP-8037 deployed-code hash word.
        executions[1] = 15_039 + gp.keccak256_word_cost(1);
        state[1] = create_cost;
        let created = BODY.create(0);
        outputs[1] = word(address_word(created));
        accounts.get_mut(&BODY).unwrap().nonce = 1;
        accounts.insert(
            created,
            Account { nonce: 1, code_hash: alloy_primitives::keccak256([0]), ..Default::default() },
        );
    } else if name == "created_then_sendall_stream" {
        // The warm zero beneficiary receives no value; SENDALL preserves the created contract.
        executions[1] = 20_170 + gp.keccak256_word_cost(2);
        state[1] = gp.create_state_gas() + gp.code_deposit_state_gas(2);
        accounts.get_mut(&BODY).unwrap().nonce = 1;
        accounts.insert(
            BODY.create(0),
            Account {
                nonce: 1,
                code_hash: alloy_primitives::keccak256([0x5f, 0xff]),
                ..Default::default()
            },
        );
    } else if name == "sstore" || name.starts_with("paid_budget_") {
        executions[1] = 15_105;
        if name.ends_with("assert_true") {
            success = false;
            statuses[2] = FrameStatus::Failure;
            executions[2] = 3_004;
        } else {
            state[1] = slot_cost;
            put_storage(&mut accounts, BODY, U256::ZERO, U256::from(42));
        }
    } else if name == "assertion_revert" {
        success = false;
        executions[1..].copy_from_slice(&[15_105, 3_004, 0]);
        statuses[2] = FrameStatus::Failure;
        statuses[3] = FrameStatus::SkippedAtomicBatch;
    } else if name == "assertion_trace" {
        executions[1..].copy_from_slice(&[15_105, 3_118]);
        state[1] = slot_cost;
        outputs[2] = word(U256::from(1));
        put_storage(&mut accounts, BODY, U256::ZERO, U256::from(42));
    } else if name.starts_with("txtrace_net_changes_")
        || name.starts_with("txtrace_scalar_rejects_")
        || name.starts_with("eventdatacopy_")
        || name == "old_eventdatacopy_byte_is_undefined"
    {
        executions[1] = 29_292 + gp.keccak256_word_cost(1);
        success = case.expected_success.unwrap();
        if success {
            let created = BODY.create(0);
            state[1] = create_cost + slot_cost;
            accounts.get_mut(&BODY).unwrap().nonce = 1;
            accounts.insert(
                created,
                Account {
                    nonce: 1,
                    code_hash: alloy_primitives::keccak256([0]),
                    ..Default::default()
                },
            );
            put_storage(&mut accounts, BODY, U256::ZERO, U256::from(42));
            let value = case.tx.frames[1].value;
            if !value.is_zero() {
                accounts.get_mut(&SENDER).unwrap().balance -= value;
                accounts.get_mut(&BODY).unwrap().balance += value;
                receipt_logs[1].push(Log::new_unchecked(
                    SYSTEM_ADDRESS,
                    vec![
                        EIP7708_TRANSFER_TOPIC,
                        B256::left_padding_from(SENDER.as_slice()),
                        B256::left_padding_from(BODY.as_slice()),
                    ],
                    word(value),
                ));
            }
            receipt_logs[1].push(Log::new_unchecked(
                BODY,
                [0x11, 0x22, 0x33, 0x44].map(|topic| B256::from(U256::from(topic))).to_vec(),
                word(U256::from(0xab)),
            ));
            if name.starts_with("txtrace_net_changes_") {
                let selector = case.other[3];
                executions[2] = 3_119;
                let mut nonce_addresses = vec![BODY, created];
                if keys.is_empty() {
                    nonce_addresses.push(SENDER);
                }
                nonce_addresses.sort_unstable();
                let mut slots = vec![(BODY, U256::ZERO, U256::from(42))];
                if let Some(&key) = keys.first() {
                    slots.push((NONCE_MANAGER, nonce_slot(SENDER, key), U256::from(1)));
                }
                slots.sort_unstable();
                let expected = match selector {
                    0 => U256::from(2),
                    1 => U256::from(slots.len()),
                    2 => U256::from(1),
                    3 => U256::from(nonce_addresses.len()),
                    4 => address_word(SENDER),
                    5 => U256::from(1_000_000_000),
                    6 => U256::from(999_999_993),
                    7 => address_word(slots[0].0),
                    8 => U256::ZERO,
                    9 => slots[0].2,
                    0x0a => slots[0].1,
                    0x0b => address_word(created),
                    0x0c => U256::from_be_bytes(alloy_primitives::KECCAK256_EMPTY.0),
                    0x0d => U256::from_be_bytes(alloy_primitives::keccak256([0]).0),
                    0x0e => address_word(nonce_addresses[0]),
                    0x0f => U256::ZERO,
                    0x10 => U256::from(1),
                    0x11 => U256::from(2),
                    0x12 => address_word(BODY),
                    0x13 => U256::from(4),
                    0x14..=0x17 => {
                        U256::from([0x11, 0x22, 0x33, 0x44][usize::from(selector - 0x14)])
                    }
                    0x18 => U256::from(32),
                    0x19 => U256::ZERO,
                    0x1a => address_word(SENDER),
                    _ => unreachable!(),
                };
                outputs[2] = word(expected);
            } else {
                let memory = U256::from_be_slice(&case.other[67..99]);
                let len = U256::from_be_slice(&case.other[1..33]).to::<usize>();
                executions[2] = 3_026 + 3 * len.div_ceil(32) as u64;
                let mut output = vec![0; 64];
                if len != 0 {
                    output[memory.to::<usize>() + 31] = 0xab;
                }
                outputs[2] = output.into();
            }
        } else {
            statuses[2] = FrameStatus::Failure;
            executions[2] = case.tx.frames[2].limits.execution;
        }
    } else if name.starts_with("txtrace_") {
        let selector = case.other[2];
        success = !matches!(selector, 4..=6 | 0x0b..=0x0d | 0x16 | 0x17 | 0x1b);
        executions[1] = 16_508;
        if success {
            executions[2] = 3_118;
            state[1] = slot_cost;
            put_storage(&mut accounts, BODY, U256::ZERO, U256::from(42));
            receipt_logs[1].push(Log::new_unchecked(
                BODY,
                vec![B256::from(U256::from(9)), B256::from(U256::from(7))],
                word(U256::from(0xab)),
            ));
            outputs[2] = word(match selector {
                0 | 2 | 8 | 0x0a | 0x0f | 0x19 => U256::ZERO,
                1 | 3 | 0x10 | 0x11 => U256::from(1),
                7 | 0x12 => address_word(BODY),
                9 => U256::from(42),
                0x0e | 0x1a => address_word(SENDER),
                0x13 => U256::from(2),
                0x14 => U256::from(9),
                0x15 => U256::from(7),
                0x18 => U256::from(32),
                _ => unreachable!(),
            });
        } else {
            statuses[2] = FrameStatus::Failure;
            executions[2] = case.tx.frames[2].limits.execution;
        }
    }
    assert_eq!(result.success, success, "{}", case.name);
    assert_eq!(result.payer, payer, "{}", case.name);
    assert_eq!(result.receipts.len(), case.tx.frames.len(), "{}", case.name);
    for (index, receipt) in result.receipts.iter().enumerate() {
        assert_eq!(receipt.status, statuses[index], "{} frame {index}", case.name);
        assert_eq!(receipt.gas_used.execution, executions[index], "{} frame {index}", case.name);
        assert_eq!(receipt.gas_used.state, state[index], "{} frame {index}", case.name);
        assert_eq!(receipt.logs, receipt_logs[index], "{} frame {index}", case.name);
    }
    assert_eq!(result.outputs, outputs, "{}", case.name);
    assert_eq!(
        result.logs,
        receipt_logs.into_iter().flatten().collect::<Vec<_>>(),
        "{}",
        case.name
    );

    // These matrices contain no signature entries. Derive the intrinsic and floor
    // directly from their wire bytes, independently of FrameGas::calculate.
    let nonce_data = nonce_calldata(keys, case.tx.nonce);
    let bytes = nonce_data.iter().chain(case.tx.frames.iter().flat_map(|frame| frame.data.iter()));
    let mut byte_count = 0;
    let mut tokens = 0;
    for byte in bytes {
        byte_count += 1;
        tokens += if *byte == 0 { 1 } else { 4 };
    }
    let base = 12_000
        + 475 * case.tx.frames.len() as u64
        + 6_000 * u64::from(!case.tx.frames.iter().all(|frame| frame.value.is_zero()))
        + 12_100 * keys.len() as u64;
    let intrinsic = base + 4 * tokens;
    let floor = base + 64 * byte_count;
    let expected_state = state.iter().sum::<u64>();
    let expected_execution = intrinsic + executions.iter().sum::<u64>();
    let expected_spent = expected_execution + expected_state;
    let refund = raw_refund.min(expected_spent / 5);
    let used = expected_execution.saturating_sub(refund).max(floor) + expected_state;
    assert_eq!(result.state_gas, expected_state, "{}", case.name);
    assert_eq!(result.spent, expected_spent, "{}", case.name);
    assert_eq!(result.floor, floor, "{}", case.name);
    assert_eq!(result.refunded, refund, "{}", case.name);
    assert_eq!(result.fee_gas, used, "{}", case.name);
    assert_eq!(result.block_execution, expected_execution.max(floor), "{}", case.name);

    let base_fee = U256::from(used) * U256::from(case.basefee);
    let tip = (U256::from(used) * case.tx.fees.max_priority_fee_per_gas)
        .min(case.tx.fees.max_fee - base_fee);
    accounts.get_mut(&payer).unwrap().balance -= base_fee + tip;
    if !tip.is_zero() {
        accounts
            .entry(Address::ZERO)
            .or_insert_with(|| Account {
                code_hash: alloy_primitives::KECCAK256_EMPTY,
                ..Default::default()
            })
            .balance += tip;
    }
    assert_eq!(result.accounts, accounts, "{}", case.name);
}

#[test]
fn native_frame_execution_matches_explicit_protocol_expectations() {
    let cases = cases();
    assert_eq!(cases.len(), 189);
    for case in &cases {
        let result = native(case);
        if case.expected_valid {
            assert_native_case(
                case,
                &result.unwrap_or_else(|error| panic!("{}: {error}", case.name)),
            );
        } else {
            assert!(result.is_err(), "{} accepted an invalid payload", case.name);
        }
    }
}

fn assertion_case(name: &str, body: &[u8], query: &[u8], success: bool) -> Case {
    let mut case = base_case(
        name,
        body,
        query,
        vec![frame(FrameMode::Sender, BODY), frame(FrameMode::Assert, OTHER)],
    );
    case.tx.frames[1].limits.state = 1_000_000;
    case.expected_success = Some(success);
    case
}

mod sequences {
    //! Deterministic sequence coverage against a small independent rollback model.

    use super::*;

    use alloy_consensus::TxEip8141;
    use alloy_eips::{
        eip8141::{Frame, FrameMode, FrameStatus},
        eip8250::{NONCE_MANAGER, nonce_slot},
    };
    use alloy_primitives::{Address, Bytes, U256};
    use common::{APPROVE_BOTH, BODY, SENDER};
    use evm2::{
        BaseEvmTypes, Evm, Inspector,
        ethereum::eip8141::FrameGas,
        evm::PendingState,
        interpreter::{InstrStop, Interpreter, Message, MessageResult},
    };
    use std::sync::{Arc, Mutex};

    const HALT: Address = Address::with_last_byte(0xb1);
    const ASSERTION: Address = Address::with_last_byte(0xc0);

    fn frame(mode: FrameMode, target: Address, flags: u8) -> Frame {
        Frame { flags, ..common::frame(mode, target) }
    }

    fn evm(assertion_fails: bool) -> Evm<'static, BaseEvmTypes> {
        // Store calldata[0] and emit a log, then revert when calldata[32] is nonzero.
        // Failed writes/logs must disappear, including when a surrounding atomic batch fails later.
        let body = [
            0x5f, 0x35, 0x5f, 0x55, 0x5f, 0x5f, 0xa0, 0x60, 0x20, 0x35, 0x15, 0x60, 0x11, 0x57,
            0x5f, 0x5f, 0xfd, 0x5b, 0x00,
        ];
        let db = common::database_with_nonce_manager([
            (SENDER, APPROVE_BOTH),
            (BODY, &body),
            (HALT, &[0xfe]),
            (ASSERTION, if assertion_fails { &[0x5f, 0x5f, 0xfd][..] } else { &[0x00] }),
        ]);
        common::evm(db)
    }

    fn stored(state: &PendingState, address: Address, key: U256) -> U256 {
        state
            .account_changes()
            .find(|account| account.address == address)
            .and_then(|account| account.storage.slots.get(&key))
            .map(|slot| slot.value.current)
            .unwrap_or_default()
    }

    #[derive(Default)]
    struct Hooks {
        calls: usize,
        ends: usize,
        frames: Vec<usize>,
    }

    struct Observe(Arc<Mutex<Hooks>>);

    impl Inspector<BaseEvmTypes> for Observe {
        fn call(
            &mut self,
            _: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _: &mut Message,
        ) -> Option<MessageResult> {
            self.0.lock().unwrap().calls += 1;
            None
        }

        fn call_end(
            &mut self,
            _: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _: &Message,
            _: &mut MessageResult,
        ) {
            self.0.lock().unwrap().ends += 1;
        }

        fn frame_end(
            &mut self,
            index: usize,
            _: &alloy_eips::eip8141::FrameReceipt,
            _: &Bytes,
            _: InstrStop,
            _: &mut Evm<'_, BaseEvmTypes>,
        ) {
            self.0.lock().unwrap().frames.push(index);
        }
    }

    #[test]
    fn generated_sequences_match_rollback_model_and_inspected_prefix_reuse() {
        let mut cases = 0;
        // Three independently chosen actions: write zero, write one, write+revert, exceptional
        // halt. Two link bits form every contiguous atomic partition of those actions.
        for actions in 0usize..64 {
            for links in 0usize..4 {
                // No assertions, two passing assertions, or first assertion fails and second is
                // skipped.
                for assertions in 0..3 {
                    for keyed in [false, true] {
                        let label = format!(
                            "actions={actions}, links={links}, assertions={assertions}, keyed={keyed}"
                        );
                        let choices = [actions & 3, (actions >> 2) & 3, (actions >> 4) & 3];
                        let mut frames = vec![frame(FrameMode::Verify, SENDER, 3)];
                        for (i, &choice) in choices.iter().enumerate() {
                            let atomic = i < 2 && links & (1 << i) != 0;
                            let mut f = frame(
                                FrameMode::Sender,
                                if choice == 3 { HALT } else { BODY },
                                if atomic { 4 } else { 0 },
                            );
                            if choice != 3 {
                                let mut data = [0; 64];
                                data[31] = if choice == 0 { 0 } else { 1 };
                                data[63] = u8::from(choice == 2);
                                f.data = Bytes::copy_from_slice(&data);
                            }
                            frames.push(f);
                        }
                        if assertions != 0 {
                            frames.extend(vec![frame(FrameMode::Assert, ASSERTION, 0); 2]);
                        }
                        let tx = common::recovered(TxEip8141 {
                            nonce_keys: Some(if keyed { vec![U256::from(1)] } else { vec![] }),
                            ..common::payload(frames)
                        });
                        let mut plain_evm = evm(assertions == 2);
                        let plain = plain_evm
                            .transact(&tx)
                            .unwrap_or_else(|error| panic!("{label}: {error:?}"))
                            .detach();
                        let mut inspected_evm = evm(assertions == 2);
                        // Admission must leave neither a nonce increment nor warmth/gas effects in
                        // the subsequent full transaction on the same EVM
                        // instance.
                        assert!(
                            inspected_evm.validate_frame_prefix(&tx, 1).unwrap().sender_approved,
                            "{label}"
                        );
                        let hooks = Arc::new(Mutex::new(Hooks::default()));
                        inspected_evm.set_inspector(Observe(hooks.clone()));
                        let inspected = inspected_evm.transact(&tx).unwrap().detach();
                        assert_eq!(plain, inspected, "{label}");

                        let outcome = plain.result.frame.as_ref().unwrap();
                        let mut statuses = vec![FrameStatus::Success; outcome.receipts.len()];
                        let mut rolled_back = vec![false; outcome.receipts.len()];
                        let mut live_logs = [false; 3];
                        let mut value = 0;
                        let mut start = 0;
                        while start < 3 {
                            let mut end = start;
                            while end < 2 && links & (1 << end) != 0 {
                                end += 1;
                            }
                            let before = value;
                            for i in start..=end {
                                if choices[i] >= 2 {
                                    statuses[i + 1] = FrameStatus::Failure;
                                    // Roll back earlier successes in this group and skip its
                                    // suffix.
                                    value = before;
                                    for j in start..i {
                                        rolled_back[j + 1] = true;
                                        live_logs[j] = false;
                                    }
                                    statuses[i + 2..end + 2].fill(FrameStatus::SkippedAtomicBatch);
                                    break;
                                }
                                value = choices[i];
                                live_logs[i] = true;
                            }
                            start = end + 1;
                        }
                        if assertions == 2 {
                            value = 0;
                            live_logs.fill(false);
                            rolled_back[1..4].fill(true);
                            statuses[4] = FrameStatus::Failure;
                            statuses[5] = FrameStatus::SkippedAtomicBatch;
                        }
                        assert_eq!(
                            stored(&plain.pending_state, BODY, U256::ZERO),
                            U256::from(value),
                            "{label}"
                        );
                        assert_eq!(
                            outcome.receipts.iter().map(|r| r.status).collect::<Vec<_>>(),
                            statuses,
                            "{label}"
                        );
                        assert_eq!(outcome.rolled_back, rolled_back, "{label}");
                        assert_eq!(
                            outcome.assertion_failure,
                            (assertions == 2).then_some(4),
                            "{label}"
                        );
                        assert_eq!(plain.result.status, assertions != 2, "{label}");
                        assert_eq!(
                            outcome.receipts.iter().map(|r| r.logs.len()).sum::<usize>(),
                            live_logs.iter().filter(|&&live| live).count(),
                            "{label}"
                        );
                        for (receipt, &rolled) in outcome.receipts.iter().zip(&rolled_back) {
                            if rolled || receipt.status != FrameStatus::Success {
                                assert_eq!(receipt.gas_used.state, 0, "{label}");
                                assert!(receipt.logs.is_empty(), "{label}");
                            }
                            if receipt.status == FrameStatus::SkippedAtomicBatch {
                                assert_eq!(receipt.gas_used.execution, 0, "{label}");
                            }
                        }
                        let intrinsic = FrameGas::calculate(
                            tx.inner().as_eip8141().unwrap(),
                            plain_evm.version(),
                        )
                        .unwrap()
                        .intrinsic;
                        assert_eq!(
                            plain.result.state_gas_spent,
                            outcome.receipts.iter().map(|r| r.gas_used.state).sum::<u64>(),
                            "{label}"
                        );
                        assert_eq!(
                            plain.result.total_gas_spent,
                            intrinsic
                                + outcome
                                    .receipts
                                    .iter()
                                    .map(|r| r.gas_used.execution + r.gas_used.state)
                                    .sum::<u64>(),
                            "{label}"
                        );
                        assert_eq!(
                            plain.pending_state.account_info(&SENDER).unwrap().nonce,
                            u64::from(!keyed),
                            "{label}"
                        );
                        if keyed {
                            assert_eq!(
                                stored(
                                    &plain.pending_state,
                                    NONCE_MANAGER,
                                    nonce_slot(SENDER, U256::from(1))
                                ),
                                U256::from(1),
                                "{label}"
                            );
                        }
                        let hooks = hooks.lock().unwrap();
                        assert_eq!(hooks.calls, hooks.ends, "{label}");
                        assert_eq!(
                            hooks.frames,
                            (0..outcome.receipts.len()).collect::<Vec<_>>(),
                            "{label}"
                        );
                        assert_eq!(
                            hooks.calls,
                            statuses
                                .iter()
                                .filter(|&&status| status != FrameStatus::SkippedAtomicBatch)
                                .count(),
                            "{label}"
                        );
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 1536);
    }
}

mod extensions {
    //! Frame execution through custom EVM families, precompiles, and inspector overrides.

    use super::*;

    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_eips::eip8141::{ENTRY_POINT, Frame, FrameMode, FrameStatus};
    use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
    use common::{APPROVE_BOTH, BODY, SENDER};
    use evm2::{
        BaseEvmConfigSelector, Evm, EvmTypesHost, Inspector, PrecompileError, PrecompileHalt,
        SpecId,
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::AccountInfo,
        interpreter::{
            GasTracker, Host, InstrStop, Interpreter, Message, MessageKind, MessageResult,
            MessageResultExt,
        },
        precompile::{PrecompileOutput, PrecompileProvider},
        precompiles::{MovePrecompileError, PrecompileId},
        registry::{FrameTransactionError, HandlerError},
    };
    use std::borrow::Cow;

    const CUSTOM: Address = Address::with_last_byte(0xf0);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Marker(u8);

    impl Default for Marker {
        fn default() -> Self {
            Self(11)
        }
    }

    #[derive(Default)]
    struct Extension {
        frames: Vec<(usize, FrameMode)>,
        calls: Vec<(u16, Address, Address)>,
        ends: Vec<(u16, InstrStop)>,
        precompiles: Vec<(u16, bool)>,
        logs: usize,
        initialized: usize,
        completed: usize,
    }

    struct CustomTypes;

    impl EvmTypesHost for CustomTypes {
        type ConfigSelector = BaseEvmConfigSelector;
        type SpecId = SpecId;
        type Tx = TxEnvelope;
        type EvmExt = Extension;
        type MessageExt = Marker;
        type MessageResultExt = Marker;
        type TxEnvExt = Marker;
        type TxResultExt = Marker;
        type BlockEnvExt = Marker;
        type Host<'a> = Evm<'a, Self>;
    }

    struct CustomPrecompiles;

    impl PrecompileProvider<CustomTypes> for CustomPrecompiles {
        fn addresses(&self) -> Vec<Address> {
            vec![CUSTOM]
        }

        fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
            vec![(CUSTOM, PrecompileId::Custom(Cow::Borrowed("frame-extension-test")))]
        }

        fn contains(&self, address: &Address) -> bool {
            *address == CUSTOM
        }

        fn move_precompiles(
            &mut self,
            moves: &[(Address, Address)],
        ) -> Result<(), MovePrecompileError> {
            match moves.iter().find(|(source, destination)| source != destination) {
                Some((source, _)) => Err(MovePrecompileError::NotAPrecompile(*source)),
                None => Ok(()),
            }
        }

        fn execute(
            &mut self,
            evm: &mut Evm<'_, CustomTypes>,
            message: &Message<CustomTypes>,
            gas: &mut GasTracker,
        ) -> Option<Result<PrecompileOutput, PrecompileError>> {
            if !self.contains(&message.call_target) {
                return None;
            }
            Some((|| {
                assert_eq!(message.ext, Marker(77));
                assert_eq!(evm.block().ext, Marker(42));
                assert_eq!(gas.state_gas_isolated(), evm.frame_transaction_active());
                let is_static = message.kind == MessageKind::StaticCall || message.caller_is_static;
                evm.ext_mut().precompiles.push((message.depth, is_static));
                gas.spend(37)?;
                if !is_static {
                    gas.spend_state(113)?;
                    evm.state_mut().storage(&CUSTOM)?.into_slot(U256::ZERO)?.set(U256::from(99));
                    evm.log(Log::new_unchecked(CUSTOM, vec![], Bytes::from_static(b"custom")));
                }
                match message.input.first().copied().unwrap_or_default() {
                    0 => Ok(PrecompileOutput::new(Bytes::from_static(b"ok"))),
                    1 => Err(PrecompileError::Revert(Bytes::from_static(b"reverted"))),
                    2 => Err(PrecompileHalt::Other(Cow::Borrowed("test halt")).into()),
                    3 => Err(PrecompileError::fatal(std::io::Error::other(
                        "custom precompile failed",
                    ))),
                    _ => unreachable!(),
                }
            })())
        }
    }

    #[derive(Default)]
    struct Observe {
        override_before: bool,
        override_after: Option<InstrStop>,
    }

    impl Inspector<CustomTypes> for Observe {
        fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, CustomTypes>) {
            assert_eq!(interp.message().ext, Marker(77));
            assert_eq!(interp.host().tx_env().ext, Marker(11));
            assert_eq!(interp.host().block().ext, Marker(42));
            interp.host().ext_mut().initialized += 1;
        }

        fn call(
            &mut self,
            interp: &mut Interpreter<'_, '_, CustomTypes>,
            message: &mut Message<CustomTypes>,
        ) -> Option<MessageResult<CustomTypes>> {
            assert_eq!(message.ext, Marker(11));
            message.ext = Marker(77);
            interp.host().ext_mut().calls.push((
                message.depth,
                message.call_target,
                message.caller,
            ));
            (self.override_before && message.depth == 1).then(|| {
                let mut gas = GasTracker::new_isolated(message.gas_limit, message.reservoir);
                gas.spend(37).unwrap();
                MessageResultExt {
                    stop: InstrStop::Return,
                    gas,
                    output: Bytes::from_static(b"override"),
                    ..Default::default()
                }
            })
        }

        fn call_end(
            &mut self,
            interp: &mut Interpreter<'_, '_, CustomTypes>,
            message: &Message<CustomTypes>,
            result: &mut MessageResult<CustomTypes>,
        ) {
            assert_eq!(message.ext, Marker(77));
            assert_eq!(result.ext, Marker(11));
            if message.depth == 1
                && let Some(stop) = self.override_after
            {
                result.stop = stop;
            }
            result.ext = Marker(88);
            interp.host().ext_mut().ends.push((message.depth, result.stop));
        }

        fn log(&mut self, _: &Log, host: &mut Evm<'_, CustomTypes>) {
            host.ext_mut().logs += 1;
        }

        fn frame_start(&mut self, index: usize, frame: &Frame, host: &mut Evm<'_, CustomTypes>) {
            host.ext_mut().frames.push((index, frame.mode));
        }

        fn frame_transaction_end(
            &mut self,
            result: &evm2::TxResult<CustomTypes>,
            host: &mut Evm<'_, CustomTypes>,
        ) {
            assert_eq!(result.ext, Marker(11));
            assert_eq!(result.frame.as_ref().unwrap().payer, SENDER);
            host.ext_mut().completed += 1;
        }
    }

    fn frame(mode: FrameMode, target: Address, selector: u8) -> Frame {
        Frame { data: Bytes::copy_from_slice(&[selector]), ..common::frame(mode, target) }
    }

    fn tx(frames: Vec<Frame>) -> Recovered<TxEnvelope> {
        common::recovered(common::payload(frames))
    }

    fn evm(body: &[u8], inspector: Observe) -> Evm<'static, CustomTypes> {
        let db = common::database_with_nonce_manager([
            (SENDER, APPROVE_BOTH),
            (BODY, body),
            (CUSTOM, &[]),
        ]);
        let mut evm = Evm::new_with_ext(
            SpecId::BOGOTA,
            BlockEnvExt::<()>::default().with_ext(Marker(42)),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            CustomPrecompiles,
            Extension::default(),
        );
        evm.set_inspector(inspector);
        evm
    }

    fn stored(evm: &mut Evm<'_, CustomTypes>) -> U256 {
        evm.state_mut().storage(&CUSTOM).unwrap().into_slot(U256::ZERO).unwrap().current()
    }

    #[test]
    fn custom_type_family_preserves_extensions_precompile_context_and_frame_hooks() {
        let mut evm = evm(&[], Observe::default());
        let tx = tx(vec![
            frame(FrameMode::Verify, SENDER, 0),
            frame(FrameMode::Default, CUSTOM, 0),
            frame(FrameMode::Sender, CUSTOM, 0),
            frame(FrameMode::Assert, CUSTOM, 0),
        ]);
        let result = evm.transact(&tx).unwrap().commit();
        assert!(result.status);
        assert_eq!(result.ext, Marker(11));
        assert_eq!(result.state_gas_spent, 226);
        let outcome = result.frame.unwrap();
        assert_eq!(
            outcome.receipts.iter().map(|receipt| receipt.gas_used.state).collect::<Vec<_>>(),
            [0, 113, 113, 0]
        );
        assert_eq!(
            outcome.receipts.iter().map(|receipt| receipt.logs.len()).collect::<Vec<_>>(),
            [0, 1, 1, 0]
        );
        assert_eq!(outcome.outputs[1..], vec![Bytes::from_static(b"ok"); 3]);
        assert_eq!(stored(&mut evm), U256::from(99));
        assert_eq!(
            evm.ext().calls,
            [
                (0, SENDER, ENTRY_POINT),
                (0, CUSTOM, ENTRY_POINT),
                (0, CUSTOM, SENDER),
                (0, CUSTOM, ENTRY_POINT)
            ]
        );
        assert_eq!(evm.ext().precompiles, [(0, false), (0, false), (0, true)]);
        assert_eq!(evm.ext().initialized, 1);
        assert_eq!(evm.ext().logs, 2);
        assert_eq!(evm.ext().ends.len(), 4);
        assert_eq!(
            evm.ext().frames,
            [
                (0, FrameMode::Verify),
                (1, FrameMode::Default),
                (2, FrameMode::Sender),
                (3, FrameMode::Assert)
            ]
        );
        assert_eq!(evm.ext().completed, 1);
    }

    #[test]
    fn custom_precompile_revert_halt_and_fatal_paths_rollback_and_allow_reuse() {
        for selector in 0..4 {
            let mut evm = evm(&[], Observe::default());
            let failed = tx(vec![
                frame(FrameMode::Verify, SENDER, 0),
                frame(FrameMode::Sender, CUSTOM, selector),
            ]);
            if selector == 3 {
                assert!(matches!(evm.transact(&failed), Err(HandlerError::Fatal(_))));
                assert_eq!(stored(&mut evm), U256::ZERO);
                assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
                assert!(evm.logs().is_empty());
                assert_eq!(evm.ext().completed, 0);
                let valid = tx(vec![
                    frame(FrameMode::Verify, SENDER, 0),
                    frame(FrameMode::Sender, CUSTOM, 0),
                ]);
                assert!(evm.transact(&valid).unwrap().commit().status);
                assert_eq!(stored(&mut evm), U256::from(99));
                continue;
            }
            let result = evm.transact(&failed).unwrap().commit();
            let receipt = &result.frame.as_ref().unwrap().receipts[1];
            assert_eq!(
                receipt.status,
                if selector == 0 { FrameStatus::Success } else { FrameStatus::Failure }
            );
            assert_eq!(receipt.gas_used.state, if selector == 0 { 113 } else { 0 });
            assert_eq!(receipt.logs.len(), usize::from(selector == 0));
            assert_eq!(stored(&mut evm), if selector == 0 { U256::from(99) } else { U256::ZERO });
            assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
            let frame = result.frame.unwrap();
            assert_eq!(
                frame.stops[1],
                match selector {
                    0 => InstrStop::Return,
                    1 => InstrStop::Revert,
                    _ => InstrStop::PrecompileError,
                }
            );
            assert_eq!(
                frame.outputs[1],
                match selector {
                    0 => Bytes::from_static(b"ok"),
                    1 => Bytes::from_static(b"reverted"),
                    _ => Bytes::new(),
                }
            );
        }
    }

    #[test]
    fn nested_inspector_overrides_settle_state_gas_and_rollback_custom_precompile_effects() {
        // CALL custom with empty data and zero value, ignore the success bit, then STOP.
        let body = [0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x60, 0xf0, 0x61, 0x27, 0x10, 0xf1, 0x00];
        let tx = tx(vec![frame(FrameMode::Verify, SENDER, 0), frame(FrameMode::Sender, BODY, 0)]);
        let mut execution_gas = None;
        for (before, after) in [
            (false, None),
            (true, None),
            (false, Some(InstrStop::Revert)),
            (false, Some(InstrStop::PrecompileError)),
        ] {
            let mut evm = evm(&body, Observe { override_before: before, override_after: after });
            let result = evm.transact(&tx).unwrap().commit();
            assert!(result.status);
            let receipt = &result.frame.as_ref().unwrap().receipts[1];
            assert_eq!(receipt.status, FrameStatus::Success);
            assert_eq!(receipt.gas_used.state, if before || after.is_some() { 0 } else { 113 });
            assert_eq!(receipt.logs.len(), usize::from(!before && after.is_none()));
            assert_eq!(
                stored(&mut evm),
                if before || after.is_some() { U256::ZERO } else { U256::from(99) }
            );
            assert_eq!(
                evm.ext().calls,
                [(0, SENDER, ENTRY_POINT), (0, BODY, SENDER), (1, CUSTOM, BODY)]
            );
            assert_eq!(
                evm.ext().ends.iter().map(|&(depth, _)| depth).collect::<Vec<_>>(),
                [0, 1, 0]
            );
            assert_eq!(evm.ext().precompiles.len(), usize::from(!before));
            assert_eq!(evm.ext().logs, usize::from(!before));
            if after.is_some_and(|stop| stop.is_halt()) {
                assert_eq!(receipt.gas_used.execution, execution_gas.unwrap() + 10_000 - 37);
            } else {
                assert_eq!(
                    *execution_gas.get_or_insert(receipt.gas_used.execution),
                    receipt.gas_used.execution
                );
            }
        }
    }

    #[test]
    fn ordinary_nested_call_end_overrides_rollback_state_logs_and_state_gas_too() {
        let body = [0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x60, 0xf0, 0x61, 0x27, 0x10, 0xf1, 0x00];
        let caller = Address::with_last_byte(0xd0);
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 200_000,
                to: TxKind::Call(BODY),
                ..Default::default()
            }),
            caller,
        );
        let mut execution_gas = None;
        for stop in [None, Some(InstrStop::Revert), Some(InstrStop::PrecompileError)] {
            let mut evm = evm(&body, Observe { override_before: false, override_after: stop });
            evm.overlay_db_mut().insert_account_info(
                &caller,
                AccountInfo::default().with_balance(U256::from(1_000_000_000)),
            );
            let result = evm.transact(&tx).unwrap().commit();
            assert!(result.status);
            assert!(result.frame.is_none());
            assert_eq!(result.state_gas_spent, if stop.is_some() { 0 } else { 113 });
            assert_eq!(result.logs.len(), usize::from(stop.is_none()));
            assert_eq!(stored(&mut evm), if stop.is_some() { U256::ZERO } else { U256::from(99) });
            assert_eq!(evm.state_mut().account(&caller).unwrap().nonce(), 1);
            assert_eq!(evm.ext().calls, [(0, BODY, caller), (1, CUSTOM, BODY)]);
            assert_eq!(evm.ext().ends.iter().map(|&(depth, _)| depth).collect::<Vec<_>>(), [1, 0]);
            assert_eq!(evm.ext().logs, 1);
            if stop.is_some_and(|stop| stop.is_halt()) {
                assert_eq!(result.execution_gas_spent(), execution_gas.unwrap() + 10_000 - 37);
            } else {
                assert_eq!(
                    *execution_gas.get_or_insert(result.execution_gas_spent()),
                    result.execution_gas_spent()
                );
            }
        }
    }

    #[test]
    fn reverted_nested_approval_cannot_authorize_the_next_sender_frame() {
        // Empty calldata approves. Nonempty calldata calls self with empty calldata, observes a
        // successful nested approval, and then reverts its enclosing default-mode frame.
        let sender = [
            0x36, 0x15, 0x60, 0x13, 0x57, 0x5f, 0x5f, 0x5f, 0x5f, 0x60, 0xa0, 0x61, 0x27, 0x10,
            0xfa, 0x50, 0x5f, 0x5f, 0xfd, 0x5b, 0x60, 3, 0x5f, 0x5f, 0xaa,
        ];
        for with_fresh_approval in [false, true] {
            let mut evm = evm(&[0x00], Observe::default());
            evm.overlay_db_mut().insert_account_info(
                &SENDER,
                AccountInfo::default()
                    .with_balance(U256::from(1_000_000_000))
                    .with_code(Bytecode::new_legacy(Bytes::copy_from_slice(&sender))),
            );
            let mut outer = frame(FrameMode::Default, SENDER, 1);
            outer.flags = 3;
            let mut frames = vec![outer];
            if with_fresh_approval {
                let mut fresh = frame(FrameMode::Verify, SENDER, 0);
                fresh.data = Bytes::new();
                frames.push(fresh);
            }
            frames.push(frame(FrameMode::Sender, BODY, 0));
            if with_fresh_approval {
                let result = evm.transact(&tx(frames)).unwrap().commit();
                let outcome = result.frame.unwrap();
                assert_eq!(
                    outcome.receipts.iter().map(|receipt| receipt.status).collect::<Vec<_>>(),
                    [FrameStatus::Failure, FrameStatus::Success, FrameStatus::Success]
                );
                assert_eq!(outcome.payer, SENDER);
                assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 1);
                assert_eq!(evm.ext().calls.len(), 4);
                assert_eq!(evm.ext().ends.len(), 4);
            } else {
                assert_eq!(
                    evm.transact(&tx(frames)).unwrap_err(),
                    HandlerError::InvalidFrameTransaction(FrameTransactionError::SenderNotApproved)
                );
                assert_eq!(evm.state_mut().account(&SENDER).unwrap().nonce(), 0);
                assert_eq!(evm.ext().calls.len(), 2);
                assert_eq!(evm.ext().ends.len(), 2);
            }
        }
    }

    #[test]
    fn setcodefrom_recent_root_address_is_an_ordinary_contract() {
        let recent_root = alloy_eips::eip8272::RECENT_ROOT_ADDRESS;
        let mut code = vec![0x73];
        code.extend_from_slice(BODY.as_slice());
        code.extend_from_slice(&[0xa7, 0x5f, 0x52, 0x60, 32, 0x5f, 0xf3]);
        let db = common::database([
            (SENDER, APPROVE_BOTH),
            (BODY, &[0x00][..]),
            (recent_root, code.as_slice()),
        ]);
        let mut evm = common::evm(db);
        let tx = common::recovered(common::payload(vec![
            common::frame(FrameMode::Verify, SENDER),
            common::frame(FrameMode::Sender, recent_root),
        ]));
        let result = evm.call_tx(&tx).unwrap();
        let frame = result.frame.unwrap();
        assert_eq!(frame.stops[1], InstrStop::Return);
        assert_eq!(frame.receipts[1].status, FrameStatus::Success);
        assert_eq!(U256::from_be_slice(&frame.outputs[1]), U256::ONE);
    }
}
