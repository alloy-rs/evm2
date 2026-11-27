use crate::utils::{AccountInfo, Bytecode, Context, TransactTo, TxEnv};
use alloy_primitives::{Address, U256, address};
use evm2::{
    evm::InMemoryDB,
    interpreter::opcode::{OpCode, op},
};
use evm2_inspectors::{
    opcode::OpcodeGasInspector,
    tracing::{TracingInspector, TracingInspectorConfig},
};

#[test]
fn opcode_gas_records_parent_call_after_child_execution() {
    let child = address!("0000000000000000000000000000000000001234");
    let parent = Address::with_last_byte(0x22);

    let parent_code = Bytecode::new_legacy(
        [
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH2,
            0x12,
            0x34,
            op::PUSH2,
            0xff,
            0xff,
            op::CALL,
            op::STOP,
        ]
        .into(),
    );

    for (child_code, child_steps, adds) in [
        (vec![op::PUSH1, 1, op::PUSH1, 1, op::ADD, op::STOP], 4, 1),
        (vec![op::INVALID], 1, 0),
        (vec![0x0c], 0, 0),
    ] {
        let context = Context::mainnet().with_db(InMemoryDB::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &child,
                AccountInfo {
                    code: Some(Bytecode::new_legacy(child_code.into())),
                    ..Default::default()
                },
            );
            db.insert_account_info(
                &parent,
                AccountInfo { code: Some(parent_code.clone()), ..Default::default() },
            );
        });

        let mut inspector = OpcodeGasInspector::new();
        let mut evm = context.build_mainnet().with_inspector(&mut inspector);
        let res = evm
            .inspect_tx(TxEnv {
                caller: Address::ZERO,
                gas_limit: 1_000_000,
                kind: TransactTo::Call(parent),
                ..Default::default()
            })
            .unwrap();
        assert!(res.result.is_success(), "{res:#?}");

        let call = OpCode::new(op::CALL).unwrap();
        let add = OpCode::new(op::ADD).unwrap();
        assert_eq!(evm.inspector().opcode_counts().get(&call), Some(&1));
        assert_eq!(evm.inspector().opcode_counts().get(&add).copied().unwrap_or_default(), adds);
        // Seven PUSHes, CALL and STOP in the parent; undefined bytes are not counted.
        assert_eq!(evm.inspector().opcode_counts().values().sum::<u64>(), 9 + child_steps);
        // The cold CALL costs 2600 regardless of how much gas its child consumes.
        assert_eq!(evm.inspector().opcode_gas().get(&call), Some(&2600));
    }
}

#[test]
fn opcode_gas_excludes_uncharged_sentry_gas() {
    let target = Address::with_last_byte(0x22);
    let code = Bytecode::new_legacy([op::PUSH1, 1, op::PUSH1, 0, op::SSTORE, op::STOP].into());
    for (gas_limit, expected_cost, success) in
        [(23_306, 0, false), (24_000, 2_994, false), (100_000, 22_100, true)]
    {
        let context = Context::mainnet().with_db(InMemoryDB::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &target,
                AccountInfo { code: Some(code.clone()), ..Default::default() },
            );
        });
        let mut inspector = OpcodeGasInspector::new();
        let mut evm = context.build_mainnet().with_inspector(&mut inspector);
        let res = evm
            .inspect_tx(TxEnv {
                caller: Address::ZERO,
                gas_limit,
                kind: TransactTo::Call(target),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(res.result.is_success(), success);
        let sstore = OpCode::new(op::SSTORE).unwrap();
        assert_eq!(evm.inspector().opcode_counts().get(&sstore), Some(&1));
        assert_eq!(evm.inspector().opcode_gas().get(&sstore), Some(&expected_cost));
    }
}

#[test]
fn opcode_gas_preserves_cold_call_out_of_gas() {
    let target = Address::with_last_byte(0x22);
    let code = Bytecode::new_legacy(
        [
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0x33,
            op::PUSH2,
            0xff,
            0xff,
            op::CALL,
            op::STOP,
        ]
        .into(),
    );
    let context = Context::mainnet().with_db(InMemoryDB::default()).modify_db_chained(|db| {
        db.insert_account_info(&target, AccountInfo { code: Some(code), ..Default::default() });
    });
    let mut inspector = OpcodeGasInspector::new();
    let mut evm = context.build_mainnet().with_inspector(&mut inspector);
    let res = evm
        .inspect_tx(TxEnv {
            caller: Address::ZERO,
            gas_limit: 21_661,
            kind: TransactTo::Call(target),
            ..Default::default()
        })
        .unwrap();
    assert!(!res.result.is_success());
    let call = OpCode::new(op::CALL).unwrap();
    assert_eq!(evm.inspector().opcode_gas().get(&call), Some(&640));
}

#[test]
fn opcode_gas_call_excludes_forwarded_gas_and_stipend() {
    const FORWARDED: u64 = 1_000;
    let parent = Address::with_last_byte(0x22);
    let target = Address::with_last_byte(0x33);

    for opcode in [op::CALL, op::CALLCODE] {
        for (value, balance) in [(0u8, 10), (1, 10), (1, 0)] {
            // A new account has no code, so the child returns at once.
            for (child, funded) in [
                (None, false),
                (Some(vec![op::STOP]), true),
                (Some(vec![op::PUSH0, op::PUSH0, op::REVERT]), true),
                (Some(vec![op::INVALID]), true),
            ] {
                let mut code = vec![op::PUSH0; 4];
                code.extend([op::PUSH1, value, op::PUSH1, 0x33, op::PUSH2, 0x03, 0xe8, opcode]);
                code.push(op::STOP);
                let context = || {
                    Context::mainnet().with_db(InMemoryDB::default()).modify_db_chained(|db| {
                        db.insert_account_info(
                            &parent,
                            AccountInfo {
                                balance: U256::from(balance),
                                code: Some(Bytecode::new_legacy(code.clone().into())),
                                ..Default::default()
                            },
                        );
                        if let Some(child) = &child {
                            db.insert_account_info(
                                &target,
                                AccountInfo {
                                    balance: U256::from(funded),
                                    code: Some(Bytecode::new_legacy(child.clone().into())),
                                    ..Default::default()
                                },
                            );
                        }
                    })
                };
                let tx = TxEnv {
                    caller: Address::ZERO,
                    gas_limit: 1_000_000,
                    kind: TransactTo::Call(parent),
                    ..Default::default()
                };

                let mut evm = context().build_mainnet_with_inspector(TracingInspector::new(
                    TracingInspectorConfig::default_geth(),
                ));
                evm.inspect_tx(tx.clone()).unwrap();
                let geth = evm.inspector().geth_builder().geth_traces(
                    0,
                    Default::default(),
                    Default::default(),
                );
                let log =
                    geth.struct_logs.iter().find(|log| log.op == "CALL" || log.op == "CALLCODE");
                let step_cost = log.unwrap().gas_cost;

                let mut inspector = OpcodeGasInspector::new();
                let mut evm = context().build_mainnet().with_inspector(&mut inspector);
                let res = evm.inspect_tx(tx).unwrap();
                assert!(res.result.is_success(), "{res:#?}");

                // The struct log step cost is the call's own cost plus the forwarded gas.
                let call = OpCode::new(opcode).unwrap();
                assert_eq!(
                    evm.inspector().opcode_gas().get(&call),
                    Some(&(step_cost - FORWARDED)),
                    "opcode {opcode:#x} value {value} balance {balance} child {child:?}"
                );
            }
        }
    }
}
