use crate::utils::{AccountInfo, Bytecode, CacheDB, Context, EmptyDB, SpecId, TransactTo, TxEnv};
use alloy_primitives::{Address, address, hex};
use evm2::interpreter::opcode::{OpCode, op};
use evm2_inspectors::opcode::OpcodeGasInspector;

#[test]
fn opcode_gas_records_parent_call_after_child_execution() {
    let child = address!("0000000000000000000000000000000000001234");
    let parent = address!("0000000000000000000000000000000000000022");

    let child_code = Bytecode::new_legacy([op::PUSH1, 1, op::PUSH1, 1, op::ADD, op::STOP].into());
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

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &child,
                AccountInfo { code: Some(child_code), ..Default::default() },
            );
            db.insert_account_info(
                &parent,
                AccountInfo { code: Some(parent_code), ..Default::default() },
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
    assert_eq!(evm.inspector().opcode_counts().get(&add), Some(&1));
    assert!(evm.inspector().opcode_gas().get(&call).copied().unwrap_or_default() > 0);
}

#[test]
fn opcode_gas_excludes_uncharged_sentry_gas() {
    let target = address!("0000000000000000000000000000000000000022");
    let code = Bytecode::new_legacy([op::PUSH1, 1, op::PUSH1, 0, op::SSTORE, op::STOP].into());
    for (gas_limit, expected_cost, success) in
        [(23_306, 0, false), (24_000, 2_994, false), (100_000, 22_100, true)]
    {
        let context =
            Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
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
    let target = address!("0000000000000000000000000000000000000022");
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
    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
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
fn opcode_gas_excludes_failed_memory_expansion_charge() {
    let contract = address!("0000000000000000000000000000000000000022");
    // The mainnet failure starts with PUSH1 0x80; PUSH1 0x40; MSTORE and 12 gas.
    // MSTORE pays its base cost, but cannot expand empty memory to three words.
    for (opcode, code, execution_gas, expected_cost) in [
        (op::MSTORE, vec![op::PUSH1, 0x80, op::PUSH1, 0x40, op::MSTORE], 12, 3),
        (op::MSTORE8, vec![op::PUSH1, 0x80, op::PUSH1, 0x40, op::MSTORE8], 12, 3),
        (op::MLOAD, vec![op::PUSH1, 0x40, op::MLOAD], 9, 3),
        // Mainnet CALLDATACOPY failures after expanding memory to 465/329 words.
        // Copying 23 words costs 72 gas, leaving 40/94 for expansion costing 112/100.
        (op::CALLDATACOPY, hex!("60006139e4526102e06117a4613a0437").to_vec(), 1947, 72),
        (op::CALLDATACOPY, hex!("60006128e4526102e0610d6461290437").to_vec(), 1382, 72),
        // A failed base charge still consumes the available gas before step_end.
        (op::MSTORE, vec![op::PUSH1, 0x80, op::PUSH1, 0x40, op::MSTORE], 8, 2),
    ] {
        let context = Context::mainnet()
            .with_db(CacheDB::<EmptyDB>::default())
            .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
            .modify_db_chained(|db| {
                db.insert_account_info(
                    &contract,
                    AccountInfo {
                        code: Some(Bytecode::new_legacy(code.into())),
                        ..Default::default()
                    },
                );
            });
        let mut inspector = OpcodeGasInspector::new();
        let mut evm = context.build_mainnet().with_inspector(&mut inspector);
        let gas_limit = 21_000 + execution_gas;
        let res = evm
            .inspect_tx(TxEnv { gas_limit, kind: TransactTo::Call(contract), ..Default::default() })
            .unwrap();

        assert!(!res.result.is_success());
        assert_eq!(res.result.tx_gas_used(), gas_limit);
        let opcode = OpCode::new(opcode).unwrap();
        assert_eq!(inspector.opcode_counts().get(&opcode), Some(&1));
        assert_eq!(inspector.opcode_gas().get(&opcode), Some(&expected_cost));
    }
}
