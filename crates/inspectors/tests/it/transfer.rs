//! Transfer tests

use crate::utils::{
    AccountInfo, Bytecode, CacheDB, Context, DatabaseCommit, EmptyDB, ExecutionResult, Output,
    SpecId, TestDbExt, TransactTo, TxEnv,
};
use alloy_primitives::{Address, B256, Log, U256, address, b256, hex};
use evm2_inspectors::{
    tracing::{TracingInspector, TracingInspectorConfig},
    transfer::{
        TRANSFER_EVENT_TOPIC, TRANSFER_LOG_EMITTER, TransferInspector, TransferKind,
        TransferOperation,
    },
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
    // `recipient` with 2 wei. `reverter` forwards 1 wei to `recipient` and then reverts.
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
            code: Some(Bytecode::new_legacy(
                hex!("6000600060006000600160ff5af15060006000fd").into(),
            )),
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
    assert_eq!(
        evm.inspector().logs(),
        &[transfer(deployer, caller, 10), transfer(caller, recipient, 2)]
    );
}

#[test]
fn discards_failed_internal_create_transfer_and_log() {
    let deployer = Address::ZERO;
    let factory = address!("0x1000000000000000000000000000000000000000");

    // Creates a contract with 1 wei and init code `INVALID`.
    let factory_code = hex!("60fe600053600160006001f05000");
    let mut db = CacheDB::new(EmptyDB::default());
    db.load_account(deployer).info.balance = U256::from(u64::MAX);
    db.insert_account_info(
        &factory,
        AccountInfo { code: Some(Bytecode::new_legacy(factory_code.into())), ..Default::default() },
    );

    let context = Context::mainnet().with_db(db).modify_cfg_chained(|c| c.spec = SpecId::LONDON);
    let mut evm =
        context.build_mainnet_with_inspector(TransferInspector::new(false).with_logs(true));
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Call(factory),
            value: U256::from(10),
            nonce: 0,
            ..Default::default()
        })
        .unwrap();

    assert!(res.result.is_success());
    assert_eq!(
        evm.inspector().transfers(),
        &[TransferOperation {
            kind: TransferKind::Call,
            from: deployer,
            to: factory,
            value: U256::from(10),
        }]
    );
    assert_eq!(evm.inspector().logs(), &[transfer(deployer, factory, 10)]);
}

#[test]
fn discards_internal_create_transfer_on_nonce_overflow() {
    let deployer = Address::ZERO;
    let factory = address!("0x1000000000000000000000000000000000000000");

    // Creates a contract with 1 wei from a factory whose nonce cannot be bumped.
    let factory_code = hex!("60fe600053600160006001f05000");
    let mut db = CacheDB::new(EmptyDB::default());
    db.load_account(deployer).info.balance = U256::from(u64::MAX);
    db.insert_account_info(
        &factory,
        AccountInfo {
            nonce: u64::MAX,
            code: Some(Bytecode::new_legacy(factory_code.into())),
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
            kind: TransactTo::Call(factory),
            value: U256::from(10),
            nonce: 0,
            ..Default::default()
        })
        .unwrap();

    assert!(res.result.is_success());
    assert_eq!(
        evm.inspector().transfers(),
        &[TransferOperation {
            kind: TransferKind::Call,
            from: deployer,
            to: factory,
            value: U256::from(10),
        }]
    );
    assert_eq!(evm.inspector().logs(), &[transfer(deployer, factory, 10)]);
}

const fn a(n: u64) -> Address {
    let mut bytes = [0; 20];
    let (_, low) = bytes.split_at_mut(12);
    let n = n.to_be_bytes();
    let mut i = 0;
    while i < 8 {
        low[i] = n[i];
        i += 1;
    }
    Address::new(bytes)
}

fn transfer(from: Address, to: Address, value: u64) -> Log {
    Log::new_unchecked(
        TRANSFER_LOG_EMITTER,
        vec![TRANSFER_EVENT_TOPIC, from.into_word(), to.into_word()],
        U256::from(value).to_be_bytes::<32>().into(),
    )
}

/// A log returned by `go-ethereum`.
enum ExpectedLog {
    Transfer(Address, Address, u64),
    Emitted(Address, &'static [B256], &'static [u8]),
}

impl ExpectedLog {
    fn to_log(&self) -> Log {
        match *self {
            Self::Transfer(from, to, value) => transfer(from, to, value),
            Self::Emitted(address, topics, data) => {
                Log::new_unchecked(address, topics.to_vec(), data.to_vec().into())
            }
        }
    }
}

/// A call executed on `go-ethereum`'s `eth_simulateV1` with `traceTransfers` enabled, and the logs
/// it returned.
struct TransferCase {
    name: &'static str,
    /// Address, code, balance and nonce.
    accounts: &'static [(Address, &'static [u8], u64, u64)],
    to: Option<Address>,
    value: u64,
    input: &'static [u8],
    expected: &'static [ExpectedLog],
}

/// Logs returned by `go-ethereum` a0cba163 (`geth --dev`, Osaka) for the same calls.
#[test]
fn simulate_transfer_logs_match_geth() {
    let sender = a(0x1001);
    for case in CASES {
        let mut db = CacheDB::new(EmptyDB::default());
        db.load_account(sender).info.balance = U256::from(10u128.pow(18));
        for &(address, code, balance, nonce) in case.accounts {
            db.insert_account_info(
                &address,
                AccountInfo {
                    balance: U256::from(balance),
                    nonce,
                    code: Some(Bytecode::new_legacy(code.to_vec().into())),
                    ..Default::default()
                },
            );
        }

        let context = Context::mainnet().with_db(db).modify_cfg_chained(|c| c.spec = SpecId::OSAKA);
        let mut evm =
            context.build_mainnet_with_inspector(TransferInspector::new(false).with_logs(true));
        let res = evm
            .inspect_tx(TxEnv {
                caller: sender,
                gas_limit: 5_000_000,
                kind: case.to.map_or(TransactTo::Create, TransactTo::Call),
                value: U256::from(case.value),
                data: case.input.to_vec().into(),
                nonce: 0,
                ..Default::default()
            })
            .unwrap();

        let expected = case.expected.iter().map(ExpectedLog::to_log).collect::<Vec<_>>();
        assert_eq!(evm.inspector().logs(), expected, "{}", case.name);
        let transfers = case
            .expected
            .iter()
            .filter_map(|log| match *log {
                ExpectedLog::Transfer(from, to, value) => Some((from, to, U256::from(value))),
                ExpectedLog::Emitted(..) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            evm.inspector().iter().map(|t| (t.from, t.to, t.value)).collect::<Vec<_>>(),
            transfers,
            "{}",
            case.name
        );
        // `go-ethereum` keeps the transfer logs in the tracer, so they are not in the receipt.
        let receipt_logs = expected
            .into_iter()
            .filter(|log| log.address != TRANSFER_LOG_EMITTER)
            .collect::<Vec<_>>();
        assert_eq!(res.tx_result.result.logs, receipt_logs, "{}", case.name);
    }
}

/// Once EIP-7708 is active the EVM logs the transfers itself, and `go-ethereum` stops tracing them.
#[test]
fn simulate_transfer_logs_use_eip7708_logs() {
    let sender = a(0x1001);
    let contract = a(0xc6);
    let mut db = CacheDB::new(EmptyDB::default());
    db.load_account(sender).info.balance = U256::from(10u128.pow(18));
    db.insert_account_info(
        &contract,
        AccountInfo {
            balance: U256::from(7),
            code: Some(Bytecode::new_legacy(
                hex!("73000000000000000000000000000000000000a002ff").into(),
            )),
            ..Default::default()
        },
    );

    let context = Context::mainnet().with_db(db).modify_cfg_chained(|c| c.spec = SpecId::AMSTERDAM);
    let mut evm =
        context.build_mainnet_with_inspector(TransferInspector::new(false).with_logs(true));
    let res = evm
        .inspect_tx(TxEnv {
            caller: sender,
            gas_limit: 5_000_000,
            kind: TransactTo::Call(contract),
            value: U256::from(5),
            nonce: 0,
            ..Default::default()
        })
        .unwrap();

    assert!(res.result.is_success());
    assert_eq!(evm.inspector().transfers().len(), 2);
    assert_eq!(res.tx_result.result.logs.len(), 2);
    assert_eq!(evm.inspector().logs(), res.tx_result.result.logs);
}

const CASES: &[TransferCase] = &[
    TransferCase {
        name: "tx_value",
        accounts: &[(a(0xc1), &hex!(""), 0, 0)],
        to: Some(a(0xc1)),
        value: 10,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0x1001), a(0xc1), 10)],
    },
    TransferCase {
        name: "tx_value_zero",
        accounts: &[(a(0xc1), &hex!(""), 0, 0)],
        to: Some(a(0xc1)),
        value: 0,
        input: &hex!(""),
        expected: &[],
    },
    TransferCase {
        name: "callcode_value",
        accounts: &[(
            a(0xc2),
            &hex!("6000600060006000600373000000000000000000000000000000000000a00361fffff25000"),
            100,
            0,
        )],
        to: Some(a(0xc2)),
        value: 0,
        input: &hex!(""),
        expected: &[],
    },
    TransferCase {
        name: "delegatecall_log",
        accounts: &[
            (
                a(0xc4),
                &hex!("60006000600060007300000000000000000000000000000000000000c561fffff450"),
                0,
                0,
            ),
            (a(0xc5), &hex!("601160006000a1"), 0, 0),
        ],
        to: Some(a(0xc4)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Emitted(
            a(0xc4),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000011")],
            &hex!(""),
        )],
    },
    TransferCase {
        name: "selfdestruct_balance",
        accounts: &[(a(0xc6), &hex!("73000000000000000000000000000000000000a002ff"), 7, 0)],
        to: Some(a(0xc6)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0xc6), a(0xa002), 7)],
    },
    TransferCase {
        name: "selfdestruct_tx_value",
        accounts: &[(a(0xc6), &hex!("73000000000000000000000000000000000000a002ff"), 7, 0)],
        to: Some(a(0xc6)),
        value: 5,
        input: &hex!(""),
        expected: &[
            ExpectedLog::Transfer(a(0x1001), a(0xc6), 5),
            ExpectedLog::Transfer(a(0xc6), a(0xa002), 12),
        ],
    },
    TransferCase {
        name: "selfdestruct_zero",
        accounts: &[(a(0xc7), &hex!("73000000000000000000000000000000000000a002ff"), 0, 0)],
        to: Some(a(0xc7)),
        value: 0,
        input: &hex!(""),
        expected: &[],
    },
    TransferCase {
        name: "selfdestruct_self",
        accounts: &[(a(0xc8), &hex!("7300000000000000000000000000000000000000c8ff"), 4, 0)],
        to: Some(a(0xc8)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0xc8), a(0xc8), 4)],
    },
    TransferCase {
        name: "selfdestruct_reverted_frame",
        accounts: &[
            (
                a(0xc9),
                &hex!(
                    "600060006000600060027300000000000000000000000000000000000000ca61fffff1506000600060006000600173000000000000000000000000000000000000a00161fffff15000"
                ),
                50,
                0,
            ),
            (
                a(0xca),
                &hex!(
                    "600060006000600060007300000000000000000000000000000000000000c661fffff15060006000fd"
                ),
                50,
                0,
            ),
            (a(0xc6), &hex!("73000000000000000000000000000000000000a002ff"), 7, 0),
        ],
        to: Some(a(0xc9)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0xc9), a(0xa001), 1)],
    },
    TransferCase {
        name: "logs_interleaved",
        accounts: &[
            (
                a(0xcb),
                &hex!(
                    "602160006000a16000600060006000600173000000000000000000000000000000000000a00161fffff15060006000a0600060006000600060017300000000000000000000000000000000000000cc61fffff1506000600060006000600273000000000000000000000000000000000000a00161fffff150602260006000a1"
                ),
                50,
                0,
            ),
            (
                a(0xcc),
                &hex!(
                    "602360006000a16000600060006000600173000000000000000000000000000000000000a00161fffff15060006000fd"
                ),
                50,
                0,
            ),
        ],
        to: Some(a(0xcb)),
        value: 0,
        input: &hex!(""),
        expected: &[
            ExpectedLog::Emitted(
                a(0xcb),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000021")],
                &hex!(""),
            ),
            ExpectedLog::Transfer(a(0xcb), a(0xa001), 1),
            ExpectedLog::Emitted(a(0xcb), &[], &hex!("")),
            ExpectedLog::Transfer(a(0xcb), a(0xa001), 2),
            ExpectedLog::Emitted(
                a(0xcb),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000022")],
                &hex!(""),
            ),
        ],
    },
    TransferCase {
        name: "toplevel_revert",
        accounts: &[(
            a(0xcd),
            &hex!(
                "603160006000a16000600060006000600173000000000000000000000000000000000000a00161fffff15060006000fd"
            ),
            50,
            0,
        )],
        to: Some(a(0xcd)),
        value: 3,
        input: &hex!(""),
        expected: &[],
    },
    TransferCase {
        name: "create_tx_value",
        accounts: &[],
        to: None,
        value: 10,
        input: &hex!("600060005360016000f3"),
        expected: &[ExpectedLog::Transfer(
            a(0x1001),
            address!("0x3817e247023b4f489352758397040b1fd33b300a"),
            10,
        )],
    },
    TransferCase {
        name: "nested_create",
        accounts: &[(a(0xce), &hex!("6000600053600160006005f05000"), 50, 0)],
        to: Some(a(0xce)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(
            a(0xce),
            address!("0x351888951695dc68547cfda22ae5d8bae8ab3b7a"),
            5,
        )],
    },
    TransferCase {
        name: "nested_create2",
        accounts: &[(a(0xcf), &hex!("60006000536007600160006003f55000"), 50, 0)],
        to: Some(a(0xcf)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(
            a(0xcf),
            address!("0xbc77ab833843214da7525df5458cc0a1392deee5"),
            3,
        )],
    },
    TransferCase {
        name: "nested_create_failed",
        accounts: &[(a(0xd0), &hex!("60fe600053600160006005f05060006000a000"), 50, 0)],
        to: Some(a(0xd0)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Emitted(a(0xd0), &[], &hex!(""))],
    },
    TransferCase {
        name: "insufficient_balance_call",
        accounts: &[(
            a(0xd1),
            &hex!(
                "60006000600060006103e873000000000000000000000000000000000000a00161fffff15060006000a0"
            ),
            50,
            0,
        )],
        to: Some(a(0xd1)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Emitted(a(0xd1), &[], &hex!(""))],
    },
    TransferCase {
        name: "precompile_value",
        accounts: &[(
            a(0xd2),
            &hex!("6000600060006000600173000000000000000000000000000000000000000461fffff15000"),
            50,
            0,
        )],
        to: Some(a(0xd2)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0xd2), a(0x4), 1)],
    },
    TransferCase {
        name: "eoa_and_zero",
        accounts: &[(
            a(0xd3),
            &hex!(
                "6000600060006000600073000000000000000000000000000000000000a00161fffff1506000600060006000600173000000000000000000000000000000000000a00161fffff150600060006000600073000000000000000000000000000000000000a00161fffffa5000"
            ),
            50,
            0,
        )],
        to: Some(a(0xd3)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0xd3), a(0xa001), 1)],
    },
    TransferCase {
        name: "callcode_selfdestruct_value",
        accounts: &[
            (
                a(0xd4),
                &hex!(
                    "600060006000600060017300000000000000000000000000000000000000c662030000f25000"
                ),
                50,
                0,
            ),
            (a(0xc6), &hex!("73000000000000000000000000000000000000a002ff"), 7, 0),
            (a(0xa002), &hex!(""), 1, 0),
        ],
        to: Some(a(0xd4)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Transfer(a(0xd4), a(0xa002), 50)],
    },
    TransferCase {
        name: "create_tx_failed",
        accounts: &[],
        to: None,
        value: 10,
        input: &hex!("fe"),
        expected: &[],
    },
    TransferCase {
        name: "nested_create_nonce_overflow",
        accounts: &[(
            a(0xd5),
            &hex!("6000600053600160006005f05060006000a000"),
            50,
            18446744073709551615,
        )],
        to: Some(a(0xd5)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Emitted(a(0xd5), &[], &hex!(""))],
    },
    TransferCase {
        name: "invalid_child",
        accounts: &[
            (
                a(0xd6),
                &hex!(
                    "600060006000600060007300000000000000000000000000000000000000d761fffff15060006000a000"
                ),
                50,
                0,
            ),
            (
                a(0xd7),
                &hex!(
                    "604160006000a16000600060006000600173000000000000000000000000000000000000a00161fffff150fe"
                ),
                50,
                0,
            ),
        ],
        to: Some(a(0xd6)),
        value: 0,
        input: &hex!(""),
        expected: &[ExpectedLog::Emitted(a(0xd6), &[], &hex!(""))],
    },
    TransferCase {
        name: "create_tx_code_too_large",
        accounts: &[],
        to: None,
        value: 10,
        input: &hex!("6160015ff3"),
        expected: &[],
    },
];
