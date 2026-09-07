use crate::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    env::{BlockEnvExt, TxEnvExt},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{Host, InstrStop, MessageExt, Word, op},
    registry::TxRegistry,
    test_utils::{legacy_bytecode, push},
};
use alloc::vec::Vec;
use alloy_primitives::Address;

type TestEvm = Evm<'static, BaseEvmTypes>;

fn run_tx(evm: &mut TestEvm, destination: Address, code: impl Into<Vec<u8>>) {
    let mut message = MessageExt {
        destination,
        code_address: destination,
        gas_limit: 100_000,
        code: legacy_bytecode(code),
        ..Default::default()
    };
    let result = Host::execute_message(evm, &TxEnvExt::default(), &mut message);
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
        evm.state.storage_slot(&contract, Word::from(1), false).unwrap().current(),
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

    assert_eq!(
        evm.state.storage_slot(&contract, Word::from(1), false).unwrap().current(),
        Word::from(7)
    );
    assert_eq!(
        evm.state.storage_slot(&contract, Word::from(2), false).unwrap().current(),
        Word::from(42)
    );
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
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message);

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
    let result = Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message);

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
