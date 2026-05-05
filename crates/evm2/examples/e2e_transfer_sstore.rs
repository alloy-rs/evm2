//! Runs a transaction that transfers value and writes storage.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use evm2::{
    BaseEvmTypes, BasePrecompiles, Evm, SpecId,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
    interpreter::op,
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

    let mut evm = Evm::<BaseEvmTypes<RecoveredTxEnvelope>>::new(
        SpecId::FRONTIER,
        BlockEnv::default(),
        ethereum_tx_registry(),
        database,
        BasePrecompiles::base(SpecId::FRONTIER),
    );
    let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
        TxLegacy {
            gas_limit: 100_000,
            gas_price: 1,
            to: TxKind::Call(contract),
            value: U256::from(7),
            ..TxLegacy::default()
        },
        caller,
    ));
    let result = evm.transact(&tx).expect("sample legacy transaction should execute");

    println!(
        "status={} gas_used={} storage[1]={}",
        result.status,
        result.gas_used,
        evm.state().account_ref(contract).expect("sample contract account should exist").storage
            [&U256::from(1)]
            .current
    );
}
