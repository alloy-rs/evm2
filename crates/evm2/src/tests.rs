use crate::{
    BaseEvmTypes, Evm,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{Host, InstrStop, Message, Word, op},
    registry::TxRegistry,
};
use alloy_primitives::{Address, Bytes};

type TestEvm = Evm<BaseEvmTypes>;

fn run_tx(evm: &mut TestEvm, destination: Address, code: impl Into<Vec<u8>>) {
    let message = Message {
        destination,
        code_address: destination,
        gas_limit: 100_000,
        ..Default::default()
    };
    let result = Host::execute_message(
        evm,
        TxEnv::default(),
        Bytecode::new_legacy(Bytes::from(code.into())),
        message,
        false,
    );
    assert!(result.stop.is_success());
}

#[test]
fn evm_executes_storage_transaction() {
    let contract = Address::from([0x11; 20]);
    let mut evm = TestEvm::new(
        BlockEnv::default(),
        TxRegistry::new(),
        InMemoryDB::default(),
        Default::default(),
    );

    run_tx(&mut evm, contract, [op::PUSH1, 0x2a, op::PUSH1, 0x01, op::SSTORE, op::STOP]);

    assert_eq!(
        evm.state().account_ref(contract).unwrap().storage.get(&Word::from(1)).unwrap().current,
        Word::from(0x2a)
    );
}

#[test]
fn evm_runs_transactions_against_initial_state() {
    let contract = Address::from([0x22; 20]);
    let mut database = InMemoryDB::default();
    database.insert_account_info(contract, AccountInfo { nonce: 1, ..Default::default() });
    database.insert_account_storage(contract, Word::from(1), Word::from(40));
    let mut evm =
        TestEvm::new(BlockEnv::default(), TxRegistry::new(), database, Default::default());

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

    let account = evm.state().account_ref(contract).unwrap();
    assert_eq!(account.storage.get(&Word::from(1)).unwrap().current, Word::from(7));
    assert_eq!(account.storage.get(&Word::from(2)).unwrap().current, Word::from(42));
}

#[test]
fn evm_reports_invalid_transaction_execution() {
    let contract = Address::from([0x33; 20]);
    let mut evm = TestEvm::new(
        BlockEnv::default(),
        TxRegistry::new(),
        InMemoryDB::default(),
        Default::default(),
    );
    let message = Message {
        destination: contract,
        code_address: contract,
        gas_limit: 100_000,
        ..Default::default()
    };
    let result = Host::execute_message(
        &mut evm,
        TxEnv::default(),
        Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0x01, op::SSTORE])),
        message,
        false,
    );

    assert_eq!(result.stop, InstrStop::StackUnderflow);
}
