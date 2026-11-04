use crate::utils::{AccountInfo, Bytecode, Context, TransactTo, TxEnv};
use alloy_primitives::{Address, address};
use evm2::{
    evm::InMemoryDB,
    interpreter::opcode::{OpCode, op},
};
use evm2_inspectors::opcode::OpcodeGasInspector;

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
