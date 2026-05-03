//! Runs a transaction that transfers value and writes storage.

use alloy_primitives::{Address, Bytes, U256};
use evm2::{
    Evm, EvmVersion,
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{AccountInfo, InMemoryDB, transaction::Transaction},
    interpreter::{SpecId, op},
    registry::TxRegistry,
};

fn main() {
    let caller = Address::from([0xaa; 20]);
    let contract = Address::from([0xbb; 20]);
    let mut database = InMemoryDB::default();
    let mut caller_info = AccountInfo::default();
    caller_info.balance = U256::from(1_000_000);
    database.insert_account_info(caller, caller_info);
    database.insert_account_info(
        contract,
        AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            0x02,
            op::PUSH1,
            0x01,
            op::SSTORE,
            op::STOP,
        ]))),
    );

    let mut evm = Evm::<EvmVersion<(), { SpecId::FRONTIER as u8 }>>::with_database(
        BlockEnv::default(),
        TxRegistry::new(),
        database,
    );
    let result = evm
        .execute(&Transaction {
            caller,
            to: Some(contract),
            gas_limit: 100_000,
            gas_price: U256::ONE,
            value: U256::from(7),
            ..Transaction::default()
        })
        .unwrap();

    println!(
        "status={} gas_used={} storage[1]={}",
        result.is_success(),
        result.gas_used,
        evm.state().account_ref(contract).unwrap().storage[&U256::from(1)].current
    );
}
