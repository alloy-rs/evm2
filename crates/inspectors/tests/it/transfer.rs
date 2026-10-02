//! Transfer tests

use crate::utils::{
    AccountInfo, Bytecode, CacheDB, Context, DatabaseCommit, EmptyDB, ExecutionResult, Output,
    SpecId, TestDbExt, TransactTo, TxEnv,
};
use alloy_primitives::{Address, U256, address, hex};
use evm2_inspectors::{
    tracing::{TracingInspector, TracingInspectorConfig},
    transfer::{TransferInspector, TransferKind, TransferOperation},
};

#[test]
fn test_internal_transfers() {
    /*
    contract Transfer {

        function sendViaCall(address payable _to) public payable {
            (bool sent, bytes memory data) = _to.call{value: msg.value}("");
        }
    }
    */

    let code = hex!(
        "608060405234801561001057600080fd5b5060ef8061001f6000396000f3fe608060405260043610601c5760003560e01c8063830c29ae146021575b600080fd5b6030602c366004608b565b6032565b005b600080826001600160a01b03163460405160006040518083038185875af1925050503d8060008114607e576040519150601f19603f3d011682016040523d82523d6000602084013e6083565b606091505b505050505050565b600060208284031215609c57600080fd5b81356001600160a01b038116811460b257600080fd5b939250505056fea26469706673582212201654bdbf09c088897c9b02f3ba9df280b136ef99c3a05ca5a21d9a10fd912d3364736f6c634300080d0033"
    );
    let deployer = Address::ZERO;

    let db = CacheDB::new(EmptyDB::default());

    let context = Context::mainnet().with_db(db).modify_cfg_chained(|c| c.spec = SpecId::LONDON);

    let mut insp = TracingInspector::new(TracingInspectorConfig::default_geth());

    // Create contract
    let mut evm = context.build_mainnet_with_inspector(&mut insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Create,
            data: code.into(),
            ..Default::default()
        })
        .unwrap();
    let addr = match res.result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Create(_, addr) => addr.unwrap(),
            _ => panic!("Create failed"),
        },
        _ => panic!("Execution failed"),
    };
    evm.ctx().db_mut().commit(res.state);

    let acc = evm.ctx().db_mut().load_account(deployer);
    acc.info.balance = U256::from(u64::MAX);

    let tx_env = TxEnv {
        caller: deployer,
        gas_limit: 100000000,
        kind: TransactTo::Call(addr),
        data: hex!("830c29ae0000000000000000000000000000000000000000000000000000000000000000")
            .into(),
        value: U256::from(10),
        nonce: 0,
        ..Default::default()
    };

    let mut evm = evm.with_inspector(TransferInspector::new(false));

    let res = evm.inspect_tx(tx_env.clone().modify().nonce(1).build_fill()).unwrap();
    assert!(res.result.is_success());

    assert_eq!(evm.inspector().transfers().len(), 2);
    assert_eq!(
        evm.inspector().transfers()[0],
        TransferOperation {
            kind: TransferKind::Call,
            from: deployer,
            to: addr,
            value: U256::from(10),
        }
    );
    assert_eq!(
        evm.inspector().transfers()[1],
        TransferOperation {
            kind: TransferKind::Call,
            from: addr,
            to: deployer,
            value: U256::from(10),
        }
    );

    let mut evm = evm.with_inspector(TransferInspector::internal_only());
    let res = evm.inspect_tx(tx_env.modify().nonce(1).build_fill()).unwrap();
    assert!(res.result.is_success());

    assert_eq!(evm.inspector().transfers().len(), 1);
    assert_eq!(
        evm.inspector().transfers()[0],
        TransferOperation {
            kind: TransferKind::Call,
            from: addr,
            to: deployer,
            value: U256::from(10),
        }
    );
}

#[test]
fn discards_failed_create_transfer() {
    let deployer = Address::ZERO;
    let mut db = CacheDB::new(EmptyDB::default());
    db.load_account(deployer).info.balance = U256::from(u64::MAX);

    let context = Context::mainnet().with_db(db).modify_cfg_chained(|c| c.spec = SpecId::LONDON);
    let mut evm = context.build_mainnet_with_inspector(TransferInspector::new(false));
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Create,
            data: hex!("fe").into(),
            value: U256::from(10),
            nonce: 0,
            ..Default::default()
        })
        .unwrap();

    assert!(!res.result.is_success());
    assert_eq!(evm.inspector().transfers(), &[]);
}

#[test]
fn discards_failed_call_transfers_and_logs() {
    let deployer = Address::ZERO;
    let caller = address!("0x1000000000000000000000000000000000000000");
    let reverter = address!("0x2000000000000000000000000000000000000000");
    let recipient = address!("0x00000000000000000000000000000000000000ff");

    // Calls `reverter` with 1 wei, `recipient` with 100 wei (more than the balance), and
    // `recipient` with 2 wei.
    let caller_code = hex!(
        "600060006000600060017320000000000000000000000000000000000000005af150"
        "6000600060006000606460ff5af150"
        "6000600060006000600260ff5af150"
        "00"
    );
    let mut db = CacheDB::new(EmptyDB::default());
    db.load_account(deployer).info.balance = U256::from(u64::MAX);
    db.insert_account_info(
        &caller,
        AccountInfo { code: Some(Bytecode::new_legacy(caller_code.into())), ..Default::default() },
    );
    db.insert_account_info(
        &reverter,
        AccountInfo {
            code: Some(Bytecode::new_legacy(hex!("60006000fd").into())),
            ..Default::default()
        },
    );

    let context = Context::mainnet().with_db(db).modify_cfg_chained(|c| c.spec = SpecId::LONDON);
    let mut evm =
        context.build_mainnet_with_inspector(TransferInspector::new(false).with_logs(true));
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Call(caller),
            value: U256::from(10),
            nonce: 0,
            ..Default::default()
        })
        .unwrap();

    assert!(res.result.is_success());
    assert_eq!(
        evm.inspector().transfers(),
        &[
            TransferOperation {
                kind: TransferKind::Call,
                from: deployer,
                to: caller,
                value: U256::from(10),
            },
            TransferOperation {
                kind: TransferKind::Call,
                from: caller,
                to: recipient,
                value: U256::from(2),
            },
        ]
    );
    let logs = &res.tx_result.result.logs;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].topics()[2], caller.into_word());
    assert_eq!(logs[1].topics()[1], caller.into_word());
    assert_eq!(logs[1].topics()[2], recipient.into_word());
}
