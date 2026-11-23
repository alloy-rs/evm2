use crate::{
    BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Inspector, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{
        LazyTxEip7702, RecoveredTxEnvelope, TxEnvelope,
        eip7702::{AuthRefunds, Eip7702Authorization, RuntimeAuthCharges, apply_auth_list},
        ethereum_tx_registry, prepare_initial_frame,
    },
    evm::{AccountInfo, AccountLoadFlags, DbStats, InMemoryDB, SYSTEM_ADDRESS},
    interpreter::{GasTracker, Host, InstrStop, MessageExt, Word, op},
    registry::TxRegistry,
    test_utils::{legacy_bytecode, push, push_address, push_all, return_top_word},
    version::GasId,
};
use alloc::{vec, vec::Vec};
use alloy_consensus::{TxEip1559, TxEip2930, TxEip7702, TxLegacy, transaction::Recovered};
use alloy_eips::eip7702::{Authorization, SignedAuthorization};
use alloy_primitives::{Address, Log, TxKind, U256, bytes, hex};

type TestEvm = Evm<'static, BaseEvmTypes>;

fn run_tx(evm: &mut TestEvm, destination: Address, code: impl Into<Vec<u8>>) {
    let mut message = MessageExt {
        destination,
        code_address: destination,
        gas_limit: 100_000,
        code: legacy_bytecode(code),
        ..Default::default()
    };
    let result = Host::execute_message(evm, &mut message).unwrap();
    assert!(result.is_success());
}

#[test]
fn evm_executes_storage_transaction() {
    let contract = Address::repeat_byte(0x11);
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

    assert_eq!(evm.state.storage_slot(&contract, Word::ONE).unwrap().current(), Word::from(0x2a));
}

#[test]
fn evm_runs_transactions_against_initial_state() {
    let contract = Address::repeat_byte(0x22);
    let mut database = InMemoryDB::default();
    database.insert_account_info(&contract, AccountInfo { nonce: 1, ..Default::default() });
    database.insert_account_storage(&contract, &Word::ONE, &Word::from(40));
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

    assert_eq!(evm.state.storage_slot(&contract, Word::ONE).unwrap().current(), Word::from(7));
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
    database.insert_account_storage(&contract, &Word::ZERO, &Word::from(5));
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
    let result = Host::execute_message(&mut evm, &mut message).unwrap();

    assert!(result.is_success());
    assert_eq!(result.gas.refunded(), 0);
}

#[test]
fn evm_reports_invalid_transaction_execution() {
    let contract = Address::repeat_byte(0x33);
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
    let result = Host::execute_message(&mut evm, &mut message).unwrap();

    assert_eq!(result.stop, InstrStop::StackUnderflow);
}

#[test]
fn eip8037_sibling_refill_restores_parent_gas_left() {
    let parent = Address::repeat_byte(0x11);
    let setter = Address::repeat_byte(0xa1);
    let clearer = Address::repeat_byte(0xb2);
    let caller = Address::repeat_byte(0xcc);
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
    assert!(baseline.is_success());
    assert!(result.is_success());
    assert_eq!(result.state_gas_spent(), 0);
    assert_eq!(result.output.len(), 32);
    assert_eq!(result.output, baseline.output);
}

#[test]
fn eip2780_authorization_charges_no_state_gas_without_eip8037() {
    let caller = Address::repeat_byte(0xbb);
    let recipient = Address::repeat_byte(0xaa);
    let authority = Address::repeat_byte(0xcc);
    let delegate = Address::repeat_byte(0xdd);

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
            TxEip7702 {
                chain_id,
                to: recipient,
                gas_limit,
                authorization_list: vec![unsigned(Authorization {
                    chain_id: U256::ZERO,
                    address: delegate,
                    nonce: 0,
                })],
                ..Default::default()
            },
            vec![Some(authority)],
        )),
        caller,
    );

    let result = evm.transact(&tx).unwrap().detach();
    assert!(result.result.is_success());
    assert_eq!(result.result.state_gas_spent(), 0);
    let authority = result.pending_state.account_info(&authority).unwrap();
    assert_eq!(authority.nonce, 1);
    assert_eq!(authority.code_hash, Bytecode::new_eip7702(delegate).hash_slow());
}

/// Wraps an authorization with a placeholder signature, for use with a cached authority.
fn unsigned(authorization: Authorization) -> SignedAuthorization {
    SignedAuthorization::new_unchecked(authorization, 0, U256::ZERO, U256::ZERO)
}

#[test]
fn eip7702_without_eip2780_refunds_only_existing_authority_execution_gas() {
    let caller = Address::from([0xbb; 20]);
    let recipient = Address::from([0xaa; 20]);
    let existing = Address::from([0xc1; 20]);
    let stale = Address::from([0xc2; 20]);
    let delegate = Address::from([0xdd; 20]);

    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    database.insert_account_info(&existing, AccountInfo::default().with_balance(U256::from(1)));
    database.insert_account_info(&stale, AccountInfo::default().with_nonce(1));
    // Prague gas prices with EIP-8037 on and EIP-2780 off. The Amsterdam state gas prices make a
    // state-gas refund visible if one is ever credited again: 120 * 1530 per new account and
    // 23 * 1530 per delegation.
    let mut version = Version::new(SpecId::PRAGUE);
    version.features.set(EvmFeatures::EIP8037, true);
    version.gas_params.set(GasId::NewAccountState, 183_600);
    version.gas_params.set(GasId::TxEip7702PerAuthState, 35_190);
    let chain_id = version.chain_id;
    let mut evm = TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::PRAGUE),
        database,
        Precompiles::base(SpecId::PRAGUE),
    );
    let authorization =
        |nonce| unsigned(Authorization { chain_id: U256::ZERO, address: delegate, nonce });
    let tx = Recovered::new_unchecked(
        TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
            TxEip7702 {
                chain_id,
                to: recipient,
                gas_limit: 100_000,
                // The second authorization is rejected: the authority's nonce is 1, not 0.
                authorization_list: vec![authorization(0), authorization(0)],
                ..Default::default()
            },
            vec![Some(existing), Some(stale)],
        )),
        caller,
    );

    let result = evm.transact(&tx).unwrap().discard();

    assert!(result.status);
    // Intrinsic gas: 21,000 base + 2 * 25,000 per authorization. The call to the empty recipient
    // costs nothing more, so the 29,000 left over is the only gas not spent.
    assert_eq!(result.total_gas_spent, 100_000 - 29_000);
    // Only the existing, applied authority refunds execution gas: 25,000 - 12,500 = 12,500, below
    // the cap of one fifth of the gas spent. No state gas is charged or credited.
    assert_eq!(result.refunded, 12_500);
    assert_eq!(result.state_gas_spent, 0);
    assert_eq!(result.tx_gas_used(), 71_000 - 12_500);
}

fn absent_authority_evm(spec: SpecId, touched: Address) -> TestEvm {
    let mut evm = TestEvm::new(
        spec,
        BlockEnvExt::default(),
        ethereum_tx_registry(spec),
        InMemoryDB::default(),
        Precompiles::base(spec),
    );
    evm.state.account(&touched).unwrap().touch();
    evm
}

fn auth_for(authority: Address) -> CustomAuthorization {
    CustomAuthorization {
        nonce: 0,
        address: Address::with_last_byte(0xdd),
        authority: Some(authority),
    }
}

#[test]
fn apply_auth_list_refunds_touched_absent_authority() {
    let touched = Address::with_last_byte(0xa1);
    let untouched = Address::with_last_byte(0xa2);
    let mut evm = absent_authority_evm(SpecId::PRAGUE, touched);
    let mut refunds = AuthRefunds::new(evm.version());

    apply_auth_list(&mut evm, 1, [auth_for(touched), auth_for(untouched)], &mut refunds).unwrap();

    // An authority is refunded unless it is absent and untouched.
    let per_auth = evm.version().gas_params.get(GasId::TxEip7702AuthRefund);
    assert_eq!(refunds.execution_refund, per_auth);
}

#[rstest::rstest]
#[case::legacy(0)]
#[case::eip2930(1)]
#[case::eip1559(2)]
fn top_level_create_uses_sender_state_nonce(
    #[case] tx_type: u8,
    #[values(false, true)] occupied_first_address: bool,
    #[values((false, 0), (false, 9), (true, 5))] nonce_check_and_tx_nonce: (bool, u64),
) {
    let (nonce_check, nonce) = nonce_check_and_tx_nonce;
    let caller = Address::from([0xbb; 20]);
    let destination = caller.create(5);
    let first_address = caller.create(0);
    let code = legacy_bytecode([op::STOP]);
    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &caller,
        AccountInfo { nonce: 5, balance: U256::from(u64::MAX), ..Default::default() },
    );
    if occupied_first_address {
        database.insert_account_info(
            &first_address,
            AccountInfo::default().with_nonce(1).with_code(code.clone()),
        );
    }
    let mut version = Version::new(SpecId::OSAKA);
    version.features.set(EvmFeatures::NONCE_CHECK, nonce_check);
    let chain_id = version.chain_id;
    let mut evm = TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
        SpecId::OSAKA,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::OSAKA),
        database,
        Precompiles::base(SpecId::OSAKA),
    );
    // Return one zero byte as deployed STOP code.
    let input = [op::PUSH1, 1, op::PUSH1, 0, op::RETURN].into();
    let gas_limit = 100_000;
    let value = U256::from(7);
    let tx = match tx_type {
        0 => TxEnvelope::Legacy(TxLegacy {
            nonce,
            gas_limit,
            value,
            input,
            to: TxKind::Create,
            ..Default::default()
        }),
        1 => TxEnvelope::Eip2930(TxEip2930 {
            chain_id,
            nonce,
            gas_limit,
            value,
            input,
            to: TxKind::Create,
            ..Default::default()
        }),
        2 => TxEnvelope::Eip1559(TxEip1559 {
            chain_id,
            nonce,
            gas_limit,
            value,
            input,
            to: TxKind::Create,
            ..Default::default()
        }),
        _ => unreachable!(),
    };
    let result = evm.transact(&Recovered::new_unchecked(tx, caller)).unwrap().commit();
    assert!(result.status, "{result:?}");
    assert_eq!(result.created_address, Some(destination));
    let created = evm.state.account_info_untracked(&destination).unwrap().unwrap();
    assert_eq!(created.nonce, 1);
    assert_eq!(created.balance, value);
    assert_eq!(created.code_hash, code.hash_slow());
    assert_eq!(evm.state.account(&caller).unwrap().nonce(), 6);
    let first = evm.state.account(&first_address).unwrap();
    if occupied_first_address {
        assert_eq!(first.nonce(), 1);
        assert_eq!(first.code_hash(), code.hash_slow());
        assert_eq!(first.balance(), U256::ZERO);
    } else {
        assert!(first.get().is_none());
    }
}

#[test]
fn eip2780_touched_absent_authority_pays_no_new_account_state_gas() {
    let authority = Address::with_last_byte(0xa1);
    let state_gas_spent = |touched: Address| {
        let mut evm = absent_authority_evm(SpecId::AMSTERDAM, touched);
        let mut gas = GasTracker::new_with_execution_gas_and_reservoir(1_000_000, 1_000_000);
        let mut charges = RuntimeAuthCharges::new(
            evm.version(),
            &mut gas,
            Address::with_last_byte(0xb0),
            Address::with_last_byte(0xc0),
            U256::ZERO,
        );
        assert!(!apply_auth_list(&mut evm, 1, [auth_for(authority)], &mut charges).unwrap());
        (gas.state_gas_spent(), evm.version().gas_params.new_account_state_gas())
    };

    let (untouched, new_account) = state_gas_spent(Address::with_last_byte(0xff));
    let (touched, _) = state_gas_spent(authority);
    assert_eq!(untouched - touched, i64::try_from(new_account).unwrap());
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
    push_all(&mut memory_oog, [Word::ONE, Word::MAX]);
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

        assert_eq!(on.is_success(), status, "{name}");
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

    assert!(inspected.is_success());
    assert!(inspected.logs.is_empty());
    assert_eq!(
        inspector.0.iter().map(|log| log.address).collect::<Vec<_>>(),
        [SYSTEM_ADDRESS, LOG_CONTRACT]
    );
    assert_eq!(inspector.0, collected.logs);
}

#[rstest::rstest]
#[case::empty_precompile(2, Bytecode::default(), &SHA256_FOUR_BYTES, 72, 230)]
#[case::precompile_with_code(2, legacy_bytecode([op::STOP]), &SHA256_FOUR_BYTES, 72, 230)]
#[case::precompile_with_delegation(
    2, Bytecode::new_eip7702(Address::with_last_byte(0x33)), &SHA256_FOUR_BYTES, 72, 2830
)]
#[case::precompile_with_self_delegation(
    2, Bytecode::new_eip7702(Address::with_last_byte(2)), &SHA256_FOUR_BYTES, 72, 330
)]
#[case::ordinary_delegation(
    0x55, Bytecode::new_eip7702(Address::with_last_byte(0x33)),
    &hex!("000000000000000000000000000000000000000000000000000000000000002a"), 18, 5276
)]
#[case::delegation_to_precompile(
    0x55, Bytecode::new_eip7702(Address::with_last_byte(2)), &[], 0, 2755
)]
fn precompile_dispatch_uses_original_callee(
    #[case] callee: u8,
    #[case] code: Bytecode,
    #[case] expected_output: &[u8],
    #[case] root_gas: u64,
    #[case] call_gas: u64,
    #[values(
        None,
        Some(op::CALL),
        Some(op::CALLCODE),
        Some(op::DELEGATECALL),
        Some(op::STATICCALL)
    )]
    opcode: Option<u8>,
) {
    let callee = Address::with_last_byte(callee);
    let delegate = Address::with_last_byte(0x33);
    let parent = Address::with_last_byte(0x44);
    let caller = Address::with_last_byte(0xaa);
    let mut database = InMemoryDB::default();
    database.insert_account_info(&callee, AccountInfo::default().with_code(code));
    database.insert_account_info(
        &delegate,
        AccountInfo::default().with_code(legacy_bytecode(hex!("602a60005260206000f3"))),
    );

    let target = if let Some(opcode) = opcode {
        // Copy transaction input into memory, call the callee, and return its entire output.
        let mut code = vec![op::CALLDATASIZE];
        push_all(&mut code, [0, 0]);
        code.push(op::CALLDATACOPY);
        push_all(&mut code, [0, 0, 4, 0]);
        if matches!(opcode, op::CALL | op::CALLCODE) {
            push(&mut code, 0);
        }
        push_address(&mut code, &callee);
        code.extend([op::GAS, opcode, op::POP, op::RETURNDATASIZE]);
        push_all(&mut code, [0, 0]);
        code.extend([op::RETURNDATACOPY, op::RETURNDATASIZE]);
        push(&mut code, 0);
        code.push(op::RETURN);
        database
            .insert_account_info(&parent, AccountInfo::default().with_code(legacy_bytecode(code)));
        parent
    } else {
        callee
    };
    let mut evm = TestEvm::new(
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        database,
        Precompiles::base(SpecId::PRAGUE),
    );
    evm.warm_precompiles();
    evm.state.prewarm(&caller);
    evm.state.prewarm(&target);
    let mut gas = GasTracker::new(100_000);
    let mut frame = prepare_initial_frame(
        &mut evm,
        caller,
        0,
        TxKind::Call(target),
        &bytes!("01020304"),
        U256::ZERO,
        &mut gas,
    )
    .unwrap();
    let result = Host::execute_message(&mut evm, &mut frame.message).unwrap();

    assert!(result.is_success(), "{:?}", result.stop);
    assert_eq!(result.output.as_ref(), expected_output);
    // Geth's Prague gas usage includes delegation resolution even for precompile callees.
    let expected_gas = match opcode {
        None => root_gas,
        Some(op::DELEGATECALL | op::STATICCALL) => call_gas - 3,
        Some(_) => call_gas,
    };
    assert_eq!(result.gas.spent(), expected_gas);
}

// SHA-256 of the four input bytes 0x01020304.
const SHA256_FOUR_BYTES: [u8; 32] =
    hex!("9f64a747e1b97f131fabb6b447296c9b6f0201e79fb3c5356e6c77e89b6a806a");

/// Authorization type that is not [`crate::ethereum::LazyAuthorization`].
struct CustomAuthorization {
    nonce: u64,
    address: Address,
    authority: Option<Address>,
}

impl Eip7702Authorization for CustomAuthorization {
    fn chain_id(&self) -> U256 {
        U256::ZERO
    }

    fn nonce(&self) -> u64 {
        self.nonce
    }

    fn address(&self) -> Address {
        self.address
    }

    fn authority(&self) -> Option<Address> {
        self.authority
    }
}

#[test]
fn apply_auth_list_accepts_custom_filtered_authorizations() {
    let applied = Address::with_last_byte(0xa1);
    let skipped = Address::with_last_byte(0xa2);
    let delegate = Address::with_last_byte(0xdd);
    let auth = |authority, nonce| CustomAuthorization { nonce, address: delegate, authority };
    let list = [
        auth(Some(applied), 1),
        auth(Some(skipped), 1),
        // Unrecoverable authority and stale nonce are rejected.
        auth(None, 1),
        auth(Some(Address::with_last_byte(0xa3)), 5),
    ];
    let mut database = InMemoryDB::default();
    for address in [applied, skipped] {
        database.insert_account_info(&address, AccountInfo::default().with_nonce(1));
    }
    let mut evm = TestEvm::new(
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::PRAGUE),
        database,
        Precompiles::base(SpecId::PRAGUE),
    );
    let mut refunds = AuthRefunds::new(evm.version());

    let oog = apply_auth_list(
        &mut evm,
        1,
        list.into_iter().filter(|auth| auth.authority != Some(skipped)),
        &mut refunds,
    )
    .unwrap();

    assert!(!oog);
    let delegation = Bytecode::new_eip7702(delegate).hash_slow();
    assert_eq!(evm.state.account(&applied).unwrap().code_hash(), delegation);
    assert_ne!(evm.state.account(&skipped).unwrap().code_hash(), delegation);
    // Only the existing, applied authority is refunded.
    let per_auth = evm.version().gas_params.get(GasId::TxEip7702AuthRefund);
    assert_eq!(refunds.execution_refund, per_auth);
}

// Tempo TIP-1175: bytecode loads are priced separately from account loads.

const CODE_COST: u32 = 1_000;

/// Runs `entry_code` as the target of a top-level call and returns the total execution gas
/// (top-level charges included), with the bytecode load priced at `code_cost`.
fn code_load_gas_priced(
    spec: SpecId,
    code_cost: u32,
    entry_code: &[u8],
    accounts: &[(Address, Bytecode)],
    prewarm: &[Address],
) -> u64 {
    let caller = Address::with_last_byte(0xca);
    let entry = Address::with_last_byte(0xe0);
    let mut version = Version::new(spec);
    version.gas_params.set(GasId::ColdCodeAdditionalCost, code_cost);
    let mut database = InMemoryDB::default();
    database
        .insert_account_info(&caller, AccountInfo::default().with_balance(U256::from(1u64 << 60)));
    database.insert_account_info(
        &entry,
        AccountInfo::default().with_code(legacy_bytecode(entry_code.to_vec())),
    );
    for (address, code) in accounts {
        database.insert_account_info(address, AccountInfo::default().with_code(code.clone()));
    }
    let mut evm = TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(spec, version),
        spec,
        BlockEnvExt::default(),
        TxRegistry::new(),
        database,
        Precompiles::base(spec),
    );
    evm.warm_precompiles();
    evm.state.prewarm(&caller);
    evm.state.prewarm(&entry);
    for address in prewarm {
        evm.state.prewarm(address);
    }
    let mut gas = GasTracker::new(1_000_000);
    let mut frame = prepare_initial_frame(
        &mut evm,
        caller,
        0,
        TxKind::Call(entry),
        &Default::default(),
        U256::ZERO,
        &mut gas,
    )
    .unwrap();
    let result = Host::execute_message(&mut evm, &mut frame.message).unwrap();
    assert!(result.is_success(), "{:?}", result.stop);
    (1_000_000 - gas.remaining()) + result.gas.spent()
}

fn code_load_gas(
    spec: SpecId,
    code_cost: u32,
    entry_code: &[u8],
    accounts: &[(Address, Bytecode)],
    prewarm: &[Address],
) -> u64 {
    code_load_gas_priced(spec, code_cost, entry_code, accounts, prewarm)
}

/// Returns how many cold bytecode loads `entry_code` pays for, checking that Ethereum pricing
/// (zero) is unaffected.
fn cold_code_loads(
    entry_code: &[u8],
    accounts: &[(Address, Bytecode)],
    prewarm: &[Address],
) -> u64 {
    let base = code_load_gas(SpecId::PRAGUE, 0, entry_code, accounts, prewarm);
    let priced = code_load_gas(SpecId::PRAGUE, CODE_COST, entry_code, accounts, prewarm);
    let extra = priced - base;
    assert_eq!(extra % u64::from(CODE_COST), 0, "extra gas {extra}");
    extra / u64::from(CODE_COST)
}

const CONTRACT: Address = Address::with_last_byte(0xc0);
const EOA: Address = Address::with_last_byte(0xe1);
const DELEGATING: Address = Address::with_last_byte(0xd0);
const REVERTER: Address = Address::with_last_byte(0xbe);

fn code_accounts() -> Vec<(Address, Bytecode)> {
    // REVERTER loads CONTRACT's code and reverts, rolling back the warmth.
    let mut reverter = Vec::new();
    push_address(&mut reverter, &CONTRACT);
    reverter.extend([op::EXTCODESIZE, op::POP]);
    push_all(&mut reverter, [0, 0]);
    reverter.push(op::REVERT);
    vec![
        (CONTRACT, legacy_bytecode(vec![op::STOP])),
        (EOA, Bytecode::default()),
        (DELEGATING, Bytecode::new_eip7702(CONTRACT)),
        (REVERTER, legacy_bytecode(reverter)),
    ]
}

fn ext_op(code: &mut Vec<u8>, opcode: u8, address: &Address) {
    match opcode {
        op::EXTCODECOPY => {
            push_all(code, [0, 0, 0]);
            push_address(code, address);
            code.push(op::EXTCODECOPY);
        }
        op::STATICCALL | op::DELEGATECALL => {
            push_all(code, [0, 0, 0, 0]);
            push_address(code, address);
            code.extend([op::GAS, opcode, op::POP]);
        }
        op::CALL | op::CALLCODE => {
            push_all(code, [0, 0, 0, 0, 0]);
            push_address(code, address);
            code.extend([op::GAS, opcode, op::POP]);
        }
        _ => {
            push_address(code, address);
            code.extend([opcode, op::POP]);
        }
    }
}

fn program(steps: &[(u8, Address)]) -> Vec<u8> {
    let mut code = Vec::new();
    for (opcode, address) in steps {
        ext_op(&mut code, *opcode, address);
    }
    code.push(op::STOP);
    code
}

#[rstest::rstest]
#[case::extcodesize_once(&[(op::EXTCODESIZE, CONTRACT)], 1)]
#[case::extcodesize_twice_charged_once(&[(op::EXTCODESIZE, CONTRACT), (op::EXTCODESIZE, CONTRACT)], 1)]
#[case::extcodecopy(&[(op::EXTCODECOPY, CONTRACT)], 1)]
#[case::extcodehash_is_account_only(&[(op::EXTCODEHASH, CONTRACT)], 0)]
#[case::balance_is_account_only(&[(op::BALANCE, CONTRACT)], 0)]
#[case::warm_account_still_pays_code(&[(op::BALANCE, CONTRACT), (op::EXTCODESIZE, CONTRACT)], 1)]
#[case::code_then_balance(&[(op::EXTCODESIZE, CONTRACT), (op::BALANCE, CONTRACT)], 1)]
#[case::call(&[(op::CALL, CONTRACT)], 1)]
#[case::callcode(&[(op::CALLCODE, CONTRACT)], 1)]
#[case::delegatecall(&[(op::DELEGATECALL, CONTRACT)], 1)]
#[case::staticcall(&[(op::STATICCALL, CONTRACT)], 1)]
#[case::call_then_extcodecopy(&[(op::STATICCALL, CONTRACT), (op::EXTCODECOPY, CONTRACT)], 1)]
#[case::address_without_code(&[(op::EXTCODESIZE, EOA), (op::CALL, EOA)], 0)]
#[case::precompile(&[(op::EXTCODESIZE, Address::with_last_byte(2)), (op::STATICCALL, Address::with_last_byte(2))], 0)]
#[case::extcode_on_delegating_account(&[(op::EXTCODESIZE, DELEGATING), (op::EXTCODECOPY, DELEGATING)], 1)]
#[case::call_follows_delegation(&[(op::STATICCALL, DELEGATING)], 2)]
#[case::delegation_target_warmed(&[(op::STATICCALL, DELEGATING), (op::EXTCODESIZE, CONTRACT)], 2)]
#[case::reverted_load_is_rolled_back(&[(op::STATICCALL, REVERTER), (op::EXTCODESIZE, CONTRACT)], 3)]
#[case::own_code_is_prewarmed(&[(op::EXTCODESIZE, Address::with_last_byte(0xe0))], 0)]
fn tip1175_code_load_pricing(#[case] steps: &[(u8, Address)], #[case] expected: u64) {
    assert_eq!(cold_code_loads(&program(steps), &code_accounts(), &[]), expected);
}

#[test]
fn tip1175_prewarmed_code_is_free() {
    let code = program(&[(op::EXTCODESIZE, CONTRACT), (op::STATICCALL, CONTRACT)]);
    assert_eq!(cold_code_loads(&code, &code_accounts(), &[CONTRACT]), 0);
}

#[rstest::rstest]
fn tip1175_code_deployed_in_tx_is_cold(#[values(false, true)] reverted_first: bool) {
    // Initcode returning the one-byte runtime code 0x00.
    let initcode = hex!("600060005360016000f3");
    let mut code = Vec::new();
    push(&mut code, Word::from_be_slice(&initcode));
    push(&mut code, 0);
    code.push(op::MSTORE);
    if reverted_first {
        // A CREATE2 whose initcode reverts installs nothing. Rollback of installed code is
        // covered by the state tests (`rollback_removes_installed_code_and_its_warmth`).
        push(&mut code, Word::from_be_slice(&hex!("60006000fd")));
        push(&mut code, 0x40);
        code.push(op::MSTORE);
        push_all(&mut code, [7, 5, 0x40 + 27, 0]);
        code.extend([op::CREATE2, op::POP]);
    }
    push_all(&mut code, [7, 10, 22, 0]);
    code.extend([op::CREATE2, op::EXTCODESIZE]);
    // Halt unless the deployed code is one byte, so a failed deployment can't pass vacuously.
    push(&mut code, 1);
    code.push(op::EQ);
    let jumpdest = code.len() + 4;
    push(&mut code, jumpdest);
    code.extend([op::JUMPI, op::INVALID, op::JUMPDEST, op::STOP]);
    assert_eq!(code[jumpdest], op::JUMPDEST);
    // Deploying the code doesn't warm it: the first load pays the code price.
    assert_eq!(cold_code_loads(&code, &[], &[]), 1);
}

#[rstest::rstest]
#[case::eip2780_charges_target(SpecId::AMSTERDAM, 1)]
#[case::free_resolution_warms_target(SpecId::PRAGUE, 0)]
fn tip1175_top_level_delegation(#[case] spec: SpecId, #[case] expected: u64) {
    // The transaction target delegates to CONTRACT, whose code then loads its own code with
    // EXTCODESIZE: free either way, since resolving the delegation warmed it.
    let mut accounts = code_accounts();
    accounts[0].1 = legacy_bytecode(program(&[(op::EXTCODESIZE, CONTRACT)]));
    accounts.push((Address::with_last_byte(0xe0), Bytecode::new_eip7702(CONTRACT)));
    let gas = |cost| code_load_gas(spec, cost, &[], &accounts, &[]);
    assert_eq!(gas(CODE_COST) - gas(0), expected * u64::from(CODE_COST));
}

fn tip1175_host() -> TestEvm {
    tip1175_host_with(&[])
}

/// [`tip1175_host`] with `extra` accounts added to the database.
fn tip1175_host_with(extra: &[(Address, Bytecode)]) -> TestEvm {
    tip1175_host_from(extra, |database| database)
}

/// [`tip1175_host`] whose database counts its reads, for [`code_reads`].
fn tip1175_counting_host() -> TestEvm {
    tip1175_host_from(&[], DbStats::new)
}

/// Returns how many times `evm`'s counting database read bytecode.
fn code_reads(evm: &TestEvm) -> u64 {
    evm.database_as::<DbStats<InMemoryDB>>().unwrap().counts().get_code_by_hash
}

fn tip1175_host_from<D: crate::evm::DynDatabase + 'static>(
    extra: &[(Address, Bytecode)],
    wrap: impl FnOnce(InMemoryDB) -> D,
) -> TestEvm {
    let mut version = Version::new(SpecId::PRAGUE);
    version.gas_params.set(GasId::ColdCodeAdditionalCost, CODE_COST);
    let mut database = InMemoryDB::default();
    for (address, code) in code_accounts().into_iter().chain(extra.iter().cloned()) {
        database.insert_account_info(&address, AccountInfo::default().with_code(code));
    }
    TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        wrap(database),
        Precompiles::base(SpecId::PRAGUE),
    )
}

/// Loads and warms `address` without its code, as `BALANCE` would with gas to spare.
fn warm_account(evm: &mut TestEvm, address: &Address) {
    Host::load_account(evm, address, AccountLoadFlags::NO_GAS_CHECK).unwrap();
}

/// Loads `address` with its code, with `gas` left for the cold surcharges.
fn load_code(
    evm: &mut TestEvm,
    address: &Address,
    gas: u64,
) -> Result<crate::evm::AccountLoad, crate::HostError> {
    let gas_params = Host::execution_config(evm).version().gas_params;
    Host::load_account(evm, address, AccountLoadFlags::for_gas(gas, &gas_params).with_code())
}

#[test]
fn tip1175_skipped_cold_code_load_halts_without_warming() {
    let mut evm = tip1175_counting_host();
    warm_account(&mut evm, &CONTRACT);
    let reads = code_reads(&evm);
    assert!(matches!(
        load_code(&mut evm, &CONTRACT, 0),
        Err(crate::HostError::Halt(InstrStop::OutOfGas))
    ));
    // Not even the cheaper code price fits, so the code is never fetched.
    assert_eq!(code_reads(&evm), reads);
    assert!(!evm.state.account(&CONTRACT).unwrap().is_code_warm());
    // Paying for it afterwards still sees a cold load, then a warm one.
    assert!(load_code(&mut evm, &CONTRACT, u64::MAX).unwrap().is_code_cold);
    let warm = load_code(&mut evm, &CONTRACT, 0).unwrap();
    assert!(!warm.is_code_cold);
    assert_eq!(warm.code.original_bytes().as_ref(), &[op::STOP]);
}

#[rstest::rstest]
#[case::without_code(EOA)]
#[case::precompile(Address::with_last_byte(2))]
fn tip1175_free_code_load_is_never_skipped(#[case] address: Address) {
    // Give the precompile real code, so only the prewarm set keeps its code load free.
    let mut evm =
        tip1175_host_with(&[(Address::with_last_byte(2), legacy_bytecode(vec![op::STOP]))]);
    evm.warm_precompiles();
    warm_account(&mut evm, &address);
    assert!(!load_code(&mut evm, &address, 0).unwrap().is_code_cold);
}

#[test]
fn tip1175_code_load_not_skipped_when_unpriced() {
    // On Ethereum the code price is zero, so a code load never skips or tracks warmth.
    let mut evm = TestEvm::new(
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        {
            let mut database = InMemoryDB::default();
            database.insert_account_info(
                &CONTRACT,
                AccountInfo::default().with_code(legacy_bytecode(vec![op::STOP])),
            );
            database
        },
        Precompiles::base(SpecId::PRAGUE),
    );
    warm_account(&mut evm, &CONTRACT);
    let load = load_code(&mut evm, &CONTRACT, 0).unwrap();
    assert!(!load.is_code_cold);
    assert!(!evm.state.account(&CONTRACT).unwrap().is_code_warm());
}

#[rstest::rstest]
// PUSH20 (3) + EXTCODESIZE: warm access (100) + cold account (2500) + code (CODE_COST). The
// program ends right after it, so the gas must cover exactly the opcode and nothing else.
#[case::extcodesize_exact(3 + 2_600 + 1_000, true)]
#[case::extcodesize_short_by_one(3 + 2_600 + 1_000 - 1, false)]
fn tip1175_code_load_gas_boundary(#[case] gas: u64, #[case] success: bool) {
    let mut evm = tip1175_host();
    let mut code = Vec::new();
    push_address(&mut code, &CONTRACT);
    code.extend([op::EXTCODESIZE, op::STOP]);
    let mut message = MessageExt {
        destination: Address::with_last_byte(0xe0),
        code_address: Address::with_last_byte(0xe0),
        gas_limit: gas,
        code: legacy_bytecode(code),
        ..Default::default()
    };
    let result = Host::execute_message(&mut evm, &mut message).unwrap();
    assert_eq!(result.is_success(), success, "{:?}", result.stop);
    // The host refuses a load it can't pay for before charging, so an exact boundary alone
    // doesn't prove the charge: a successful run must also spend every unit of gas.
    if success {
        assert_eq!(result.gas.spent(), gas);
    }
    assert_eq!(evm.state.account(&CONTRACT).unwrap().is_code_warm(), success);
}

#[rstest::rstest]
// A delegation designator is priced as ordinary bytecode.
#[case::extcodesize(&[(op::EXTCODESIZE, DELEGATING)], 1)]
#[case::extcodesize_twice(&[(op::EXTCODESIZE, DELEGATING), (op::EXTCODECOPY, DELEGATING)], 1)]
#[case::extcodehash_is_account_only(&[(op::EXTCODEHASH, DELEGATING)], 0)]
#[case::call_pays_designator_and_target(&[(op::STATICCALL, DELEGATING)], 2)]
#[case::call_then_extcode_is_warm(
    &[(op::STATICCALL, DELEGATING), (op::EXTCODESIZE, DELEGATING), (op::EXTCODESIZE, CONTRACT)],
    2
)]
fn tip1175_delegation_designator_pricing(#[case] steps: &[(u8, Address)], #[case] expected: u64) {
    assert_eq!(cold_code_loads(&program(steps), &code_accounts(), &[]), expected);
}

#[test]
fn tip1175_skipped_designator_load_halts_without_warming() {
    let mut evm = tip1175_counting_host();
    warm_account(&mut evm, &DELEGATING);
    let reads = code_reads(&evm);
    assert!(matches!(
        load_code(&mut evm, &DELEGATING, 0),
        Err(crate::HostError::Halt(InstrStop::OutOfGas))
    ));
    assert_eq!(code_reads(&evm), reads);
    assert!(!evm.state.account(&DELEGATING).unwrap().is_code_warm());
    let load = load_code(&mut evm, &DELEGATING, u64::MAX).unwrap();
    assert!(load.is_code_cold);
    assert!(load.code.is_eip7702());
}

#[rstest::rstest]
// One gas short of the code price: the load is refused and leaves the code cold.
#[case::short_by_one(CODE_COST - 1, true)]
#[case::exact_price(CODE_COST, false)]
fn tip1175_code_load_checks_the_exact_price(#[case] gas_limit: u32, #[case] skipped: bool) {
    let mut evm = tip1175_host();
    warm_account(&mut evm, &CONTRACT);
    let load = load_code(&mut evm, &CONTRACT, u64::from(gas_limit));
    if skipped {
        assert!(matches!(load, Err(crate::HostError::Halt(InstrStop::OutOfGas))));
    } else {
        assert!(load.unwrap().is_code_cold);
    }
    assert_eq!(evm.state.account(&CONTRACT).unwrap().is_code_warm(), !skipped);
}

#[rstest::rstest]
// A cold account with cold code needs both prices: with only the account's, the load halts and
// leaves both the account and its code cold.
#[case::account_and_code(2_500 + CODE_COST, true)]
#[case::account_only(2_500 + CODE_COST - 1, false)]
#[case::nothing(2_499, false)]
fn tip1175_cold_account_and_code_load(#[case] gas: u32, #[case] loaded: bool) {
    let mut evm = tip1175_host();
    let load = load_code(&mut evm, &CONTRACT, u64::from(gas));
    if loaded {
        let load = load.unwrap();
        assert!(load.is_cold && load.is_code_cold);
        assert_eq!(
            load.cold_cost(&Host::execution_config(&evm).version().gas_params),
            u64::from(gas)
        );
    } else {
        assert!(matches!(load, Err(crate::HostError::Halt(InstrStop::OutOfGas))));
    }
    let account = evm.state.account(&CONTRACT).unwrap();
    assert_eq!(account.is_warm(), loaded);
    assert_eq!(account.is_code_warm(), loaded);
}

#[test]
fn tip1175_account_and_code_warmth_roll_back_together() {
    let mut evm = tip1175_host();
    let checkpoint = evm.state.checkpoint();
    load_code(&mut evm, &CONTRACT, u64::MAX).unwrap();
    evm.state.rollback(checkpoint, crate::EvmFeatures::empty());
    // Check before anything reloads it: `State::account` would insert a fresh entry. The
    // rollback must keep the read in the overlay, so the block access list still records it.
    assert!(evm.state.loaded_accounts().any(|(address, _)| *address == CONTRACT));
    let account = evm.state.account(&CONTRACT).unwrap();
    assert!(!account.is_warm());
    assert!(!account.is_code_warm());
}

#[test]
fn tip1175_load_flags_for_gas() {
    let mut gas_params = Version::new(SpecId::PRAGUE).gas_params;
    gas_params.set(GasId::ColdCodeAdditionalCost, CODE_COST);
    let flags = |gas| AccountLoadFlags::for_gas(gas, &gas_params).with_code();
    assert_eq!(flags(0), AccountLoadFlags::CODE);
    assert_eq!(flags(u64::from(CODE_COST)), AccountLoadFlags::CODE | AccountLoadFlags::COLD_CODE);
    assert_eq!(
        flags(2_500),
        AccountLoadFlags::CODE | AccountLoadFlags::COLD_ACCOUNT | AccountLoadFlags::COLD_CODE
    );
    assert_eq!(
        flags(2_500 + u64::from(CODE_COST)),
        AccountLoadFlags::CODE | AccountLoadFlags::NO_GAS_CHECK
    );
    assert!(!AccountLoadFlags::for_gas(u64::MAX, &gas_params).contains(AccountLoadFlags::CODE));
}

/// Builds an EIP-2780 host whose transaction target delegates to `CONTRACT`, prepares the
/// top-level frame with `gas`, and returns the gas spent, whether the frame ran out of gas
/// before executing, and whether `CONTRACT`'s code is warm.
fn tip1175_top_level_delegation_run(gas: u64) -> (u64, bool, bool) {
    let spec = SpecId::AMSTERDAM;
    let caller = Address::with_last_byte(0xca);
    let entry = Address::with_last_byte(0xe0);
    let mut version = Version::new(spec);
    version.gas_params.set(GasId::ColdCodeAdditionalCost, CODE_COST);
    let mut database = InMemoryDB::default();
    database
        .insert_account_info(&caller, AccountInfo::default().with_balance(U256::from(1u64 << 60)));
    database.insert_account_info(
        &entry,
        AccountInfo::default().with_code(Bytecode::new_eip7702(CONTRACT)),
    );
    database.insert_account_info(
        &CONTRACT,
        AccountInfo::default().with_code(legacy_bytecode(vec![op::STOP])),
    );
    let mut evm = TestEvm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(spec, version),
        spec,
        BlockEnvExt::default(),
        TxRegistry::new(),
        database,
        Precompiles::base(spec),
    );
    evm.state.prewarm(&caller);
    evm.state.prewarm(&entry);
    let mut tracker = GasTracker::new(gas);
    let frame = prepare_initial_frame(
        &mut evm,
        caller,
        0,
        TxKind::Call(entry),
        &Default::default(),
        U256::ZERO,
        &mut tracker,
    )
    .unwrap();
    let warm = evm.state.account(&CONTRACT).unwrap().is_code_warm();
    (gas - tracker.remaining(), frame.runtime_oog, warm)
}

#[test]
fn tip1175_top_level_delegation_unaffordable_code_stays_cold() {
    // With enough gas the target's code is charged at the contract price and warmed.
    let (spent, halted, warm) = tip1175_top_level_delegation_run(1_000_000);
    assert!(!halted);
    assert!(warm);
    // One gas short of the code price at the code step: the frame halts and the code must stay
    // cold.
    let budget = spent - 1;
    let (_, halted, warm) = tip1175_top_level_delegation_run(budget);
    assert!(halted, "the short budget must halt the frame before it runs");
    assert!(!warm);
}
