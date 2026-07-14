use crate::utils::{AccountInfo, Bytecode, CacheDB, Context, EmptyDB, TransactTo, TxEnv};
use alloy_primitives::{Address, address};
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
