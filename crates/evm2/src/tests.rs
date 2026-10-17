use crate::{
    BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Inspector, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{LazyTxEip7702, RecoveredTxEnvelope, TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB, SYSTEM_ADDRESS},
    interpreter::{Host, InstrStop, MessageExt, Word, op},
    registry::TxRegistry,
    test_utils::{legacy_bytecode, push, push_address, push_all},
};
use alloc::{vec, vec::Vec};
use alloy_consensus::{TxEip7702, TxLegacy, transaction::Recovered};
use alloy_eips::eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization};
use alloy_primitives::{Address, Log, TxKind, U256};

type TestEvm = Evm<'static, BaseEvmTypes>;

fn run_tx(evm: &mut TestEvm, destination: Address, code: impl Into<Vec<u8>>) {
    let mut message = MessageExt {
        destination,
        code_address: destination,
        gas_limit: 100_000,
        code: legacy_bytecode(code),
        ..Default::default()
    };
    let result = Host::execute_message(evm, &TxEnvExt::default(), &mut message).unwrap();
    assert!(result.stop.is_success());
}

#[test]
fn evm_executes_storage_transaction() {
    let contract = Address::from([0x11; 20]);
    let mut evm = TestEvm::new(
        SpecId::OSAKA,
        BlockEnvExt::default(),
        TxRegistry::new(),
        InMemoryDB::default(),
        Precompiles::base(SpecId::OSAKA),
    );

    assert!(evm.database_as::<InMemoryDB>().is_some());
    assert!(evm.database_as_mut::<InMemoryDB>().is_some());
    assert!(evm.precompiles_as::<Precompiles>().is_some());
    assert!(evm.precompiles_as_mut::<Precompiles>().is_some());
    evm.set_database(InMemoryDB::default());
    evm.set_precompiles(Precompiles::base(SpecId::OSAKA));

    run_tx(&mut evm, contract, [op::PUSH1, 0x2a, op::PUSH1, 0x01, op::SSTORE, op::STOP]);

    assert_eq!(
        evm.state.storage_slot(&contract, Word::from(1)).unwrap().current(),
        Word::from(0x2a)
    );
}

#[test]
fn evm_runs_transactions_against_initial_state() {
    let contract = Address::from([0x22; 20]);
    let mut database = InMemoryDB::default();
    database.insert_account_info(&contract, AccountInfo { nonce: 1, ..Default::default() });
    database.insert_account_storage(&contract, &Word::from(1), &Word::from(40));
    let mut evm = TestEvm::new(
        SpecId::OSAKA,
        BlockEnvExt::default(),
        TxRegistry::new(),
        database,
        Precompiles::base(SpecId::OSAKA),
    );

    run_tx(
        &mut evm,
        contract,
        [
            op::PUSH1,
            0x01,
            op::SLOAD,
            op::PUSH1,
            0x02,
            op::ADD,
            op::PUSH1,
            0x02,
            op::SSTORE,
            op::STOP,
        ],
    );
    run_tx(&mut evm, contract, [op::PUSH1, 0x07, op::PUSH1, 0x01, op::SSTORE, op::STOP]);

    assert_eq!(evm.state.storage_slot(&contract, Word::from(1)).unwrap().current(), Word::from(7));
    assert_eq!(evm.state.storage_slot(&contract, Word::from(2)).unwrap().current(), Word::from(42));
}

#[test]
fn evm_propagates_child_sstore_negative_refund() {
    let contract = Address::with_last_byte(0x44);
    let mut child_code = Vec::new();
    push(&mut child_code, 7);
    push(&mut child_code, 0);
    child_code.extend([op::SSTORE, op::STOP]);

    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &contract,
        AccountInfo { nonce: 1, code: Some(legacy_bytecode(child_code)), ..Default::default() },
    );
    database.insert_account_storage(&contract, &Word::from(0), &Word::from(5));
    let mut evm = TestEvm::new(
        SpecId::LONDON,
        BlockEnvExt::default(),
        TxRegistry::new(),
        database,
        Precompiles::base(SpecId::LONDON),
    );

    let mut parent_code = Vec::new();
    push(&mut parent_code, 0);
    push(&mut parent_code, 0);
    parent_code.push(op::SSTORE);
    push(&mut parent_code, 0); // return length
    push(&mut parent_code, 0); // return offset
    push(&mut parent_code, 0); // input length
    push(&mut parent_code, 0); // input offset
    push(&mut parent_code, 0); // value
    push(&mut parent_code, 0x44); // callee
    push(&mut parent_code, 50_000); // gas
    parent_code.extend([op::CALL, op::STOP]);

    let mut message = MessageExt {
        destination: contract,
        code_address: contract,
        gas_limit: 100_000,
        code: legacy_bytecode(parent_code),
        ..Default::default()
    };
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap();

    assert!(result.stop.is_success());
    assert_eq!(result.gas.refunded(), 0);
}

#[test]
fn evm_reports_invalid_transaction_execution() {
    let contract = Address::from([0x33; 20]);
    let mut evm = TestEvm::new(
        SpecId::OSAKA,
        BlockEnvExt::default(),
        TxRegistry::new(),
        InMemoryDB::default(),
        Precompiles::base(SpecId::OSAKA),
    );
    let mut message = MessageExt {
        destination: contract,
        code_address: contract,
        gas_limit: 100_000,
        code: legacy_bytecode([op::PUSH1, 0x01, op::SSTORE]),
        ..Default::default()
    };
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap();

    assert_eq!(result.stop, InstrStop::StackUnderflow);
}

#[test]
fn eip8037_sibling_refill_restores_parent_gas_left() {
    use crate::{
        EvmFeatures, ExecutionConfig, Version,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        version::GasId,
    };
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{TxKind, U256};

    let parent = Address::from([0x11; 20]);
    let setter = Address::from([0xa1; 20]);
    let clearer = Address::from([0xb2; 20]);
    let caller = Address::from([0xcc; 20]);
    let mut parent_code = Vec::new();
    for target in [setter, clearer] {
        // DELEGATECALL(gas, target, 0, 0, 0, 0).
        for _ in 0..4 {
            parent_code.extend([op::PUSH1, 0]);
        }
        parent_code.push(op::PUSH20);
        parent_code.extend_from_slice(target.as_slice());
        parent_code.extend([op::GAS, op::DELEGATECALL, op::POP]);
    }
    // Return the gas visible to the parent after both siblings finish.
    parent_code.extend([
        op::GAS,
        op::PUSH1,
        0,
        op::MSTORE,
        op::PUSH1,
        32,
        op::PUSH1,
        0,
        op::RETURN,
    ]);

    let mut database = InMemoryDB::default();
    for (address, code) in [
        (parent, legacy_bytecode(parent_code)),
        (setter, legacy_bytecode([op::PUSH1, 1, op::PUSH1, 0, op::SSTORE, op::STOP])),
        (clearer, legacy_bytecode([op::PUSH1, 0, op::PUSH1, 0, op::SSTORE, op::STOP])),
    ] {
        database
            .insert_account_info(&address, AccountInfo::default().with_nonce(1).with_code(code));
    }
    database.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            gas_limit: 1_000_000,
            to: TxKind::Call(parent),
            ..Default::default()
        }),
        caller,
    );
    let run = |enabled| {
        let mut version = Version::new(SpecId::AMSTERDAM);
        version.features.remove(EvmFeatures::EIP2780);
        version.features.set(EvmFeatures::EIP8037, enabled);
        version.gas_params.set(GasId::SstoreSetState, 200_000);
        let mut evm = TestEvm::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::AMSTERDAM, version),
            SpecId::AMSTERDAM,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database.clone(),
            Precompiles::base(SpecId::AMSTERDAM),
        );
        evm.transact(&tx).unwrap().discard()
    };

    let baseline = run(false);
    let result = run(true);
    assert!(baseline.status);
    assert!(result.status);
    assert_eq!(result.state_gas_spent, 0);
    assert_eq!(result.output.len(), 32);
    assert_eq!(result.output, baseline.output);
}

#[test]
fn eip2780_authorization_charges_no_state_gas_without_eip8037() {
    let caller = Address::from([0xbb; 20]);
    let recipient = Address::from([0xaa; 20]);
    let authority = Address::from([0xcc; 20]);
    let delegate = Address::from([0xdd; 20]);

    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    let mut version = Version::new(SpecId::AMSTERDAM);
    version.features.remove(EvmFeatures::EIP8037);
    let chain_id = version.chain_id;
    let mut evm = TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::AMSTERDAM, version),
        SpecId::AMSTERDAM,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::AMSTERDAM),
        database,
        Precompiles::base(SpecId::AMSTERDAM),
    );
    // TX_BASE + cold recipient + per-authorization base + the authority's ACCOUNT_WRITE.
    let gas_limit = 12_000 + 3_000 + 7_816 + 9_000;
    let tx = Recovered::new_unchecked(
        TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
            TxEip7702 { chain_id, to: recipient, gas_limit, ..Default::default() },
            vec![RecoveredAuthorization::new_unchecked(
                Authorization { chain_id: U256::ZERO, address: delegate, nonce: 0 },
                RecoveredAuthority::Valid(authority),
            )],
        )),
        caller,
    );

    let result = evm.transact(&tx).unwrap().detach();
    assert!(result.result.status);
    assert_eq!(result.result.state_gas_spent, 0);
    let authority = result.pending_state.account_info(&authority).unwrap();
    assert_eq!(authority.nonce, 1);
    assert_eq!(authority.code_hash, Bytecode::new_eip7702(delegate).hash_slow());
}

const LOG_CALLER: Address = Address::with_last_byte(0xa0);
const LOG_CONTRACT: Address = Address::with_last_byte(0xa1);
const LOG_EMITTER: Address = Address::with_last_byte(0xa2);

/// Builds an Amsterdam EVM where `LOG_CONTRACT` runs `code` and `LOG_EMITTER` emits one `LOG0`.
fn log_collection_evm(log_collection: bool, code: Vec<u8>) -> TestEvm {
    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &LOG_CALLER,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    for (address, code) in
        [(LOG_CONTRACT, code), (LOG_EMITTER, vec![op::PUSH0, op::PUSH0, op::LOG0])]
    {
        database.insert_account_info(
            &address,
            AccountInfo::default().with_nonce(1).with_code(legacy_bytecode(code)),
        );
    }
    let mut version = Version::new(SpecId::AMSTERDAM);
    version.features.set(EvmFeatures::LOG_COLLECTION, log_collection);
    TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::AMSTERDAM, version),
        SpecId::AMSTERDAM,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::AMSTERDAM),
        database,
        Precompiles::base(SpecId::AMSTERDAM),
    )
}

/// Calls `LOG_CONTRACT` with value, so EIP-7708 adds a transfer log.
fn log_collection_tx() -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            gas_limit: 1_000_000,
            to: TxKind::Call(LOG_CONTRACT),
            value: U256::from(7),
            ..Default::default()
        }),
        LOG_CALLER,
    )
}

/// Appends code that returns the word at the top of the stack.
fn return_top_word(code: &mut Vec<u8>) {
    code.extend([op::PUSH0, op::MSTORE, op::PUSH1, 32, op::PUSH0, op::RETURN]);
}

#[test]
fn log_collection_off_keeps_log_semantics() {
    // LOG4 over memory that is already allocated, then LOG0 past it, which grows memory to four
    // words. Returns MSIZE.
    let mut logs = Vec::new();
    push_all(&mut logs, [0xbeef, 0]);
    logs.push(op::MSTORE);
    push_all(&mut logs, [4, 3, 2, 1, 2, 30]);
    logs.push(op::LOG4);
    push_all(&mut logs, [33, 64]);
    logs.extend([op::LOG0, op::MSIZE]);
    return_top_word(&mut logs);

    let mut memory_oog = Vec::new();
    push_all(&mut memory_oog, [Word::from(1), Word::MAX]);
    memory_oog.push(op::LOG0);

    let mut underflow = Vec::new();
    push_all(&mut underflow, [0, 0]);
    underflow.push(op::LOG2);

    // A static call into a logging contract fails; the call's success flag is returned.
    let mut static_call = Vec::new();
    push_all(&mut static_call, [0, 0, 0, 0]);
    push_address(&mut static_call, &LOG_EMITTER);
    static_call.extend([op::GAS, op::STATICCALL]);
    return_top_word(&mut static_call);

    for (name, code, status, output, logs) in [
        ("logs", logs, true, Some(Word::from(128)), 3),
        ("memory_oog", memory_oog, false, None, 0),
        ("underflow", underflow, false, None, 0),
        ("static_call", static_call, true, Some(Word::ZERO), 1),
    ] {
        let run = |log_collection| {
            log_collection_evm(log_collection, code.clone())
                .transact(&log_collection_tx())
                .unwrap()
                .discard()
        };
        let mut on = run(true);
        let off = run(false);

        assert_eq!(on.status, status, "{name}");
        if let Some(output) = output {
            assert_eq!(on.output[..], output.to_be_bytes::<32>(), "{name}");
        }
        assert_eq!(on.logs.len(), logs, "{name}");
        assert!(off.logs.is_empty(), "{name}");
        on.logs.clear();
        assert_eq!(on, off, "{name}");
    }
}

#[test]
fn log_collection_off_still_inspects_logs() {
    #[derive(Default)]
    struct LogInspector(Vec<Log>);

    impl Inspector<BaseEvmTypes> for LogInspector {
        fn log(&mut self, log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
            self.0.push(log.clone());
        }
    }

    let code = vec![op::PUSH0, op::PUSH0, op::LOG0];
    let collected =
        log_collection_evm(true, code.clone()).transact(&log_collection_tx()).unwrap().discard();
    let mut evm = log_collection_evm(false, code);
    evm.set_inspector(LogInspector::default());
    let inspected = evm.transact(&log_collection_tx()).unwrap().discard();
    let inspector = evm.clear_inspector_as::<LogInspector>().unwrap();

    assert!(inspected.status);
    assert!(inspected.logs.is_empty());
    assert_eq!(
        inspector.0.iter().map(|log| log.address).collect::<Vec<_>>(),
        [SYSTEM_ADDRESS, LOG_CONTRACT]
    );
    assert_eq!(inspector.0, collected.logs);
}
