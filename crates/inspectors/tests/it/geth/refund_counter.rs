//! The step refund counter is the transaction-wide counter of geth's `StateDB`.

use super::*;

#[test]
fn step_refund_counter_spans_frames() {
    let caller = address!("1000000000000000000000000000000000000001");
    let parent = address!("2000000000000000000000000000000000000002");
    let child = address!("3000000000000000000000000000000000000003");

    // The parent clears slot 0, which refunds 4800, and delegate-calls code that restores the
    // slot, which removes that refund and adds 2800.
    let mut parent_code = vec![op::PUSH1, 0, op::PUSH1, 0, op::SSTORE];
    parent_code.extend([op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH20]);
    parent_code.extend(child.as_slice());
    parent_code.extend([op::GAS, op::DELEGATECALL, op::STOP]);
    let child_code = [op::PUSH1, 1, op::PUSH1, 0, op::SSTORE, op::STOP];

    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        &parent,
        AccountInfo { code: Some(Bytecode::new_raw(parent_code.into())), ..Default::default() },
    );
    db.insert_account_storage(&parent, &U256::ZERO, &U256::ONE);
    db.insert_account_info(
        &child,
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::copy_from_slice(&child_code))),
            ..Default::default()
        },
    );
    let mut evm = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
        .build_mainnet_with_inspector(
            TracingInspector::new(TracingInspectorConfig::default_geth()),
        );
    evm.inspect_tx(TxEnv {
        caller,
        gas_limit: 1_000_000,
        kind: TransactTo::Call(parent),
        ..Default::default()
    })
    .unwrap();

    let steps = |index: usize| {
        evm.inspector.traces().nodes()[index]
            .trace
            .steps
            .iter()
            .map(|step| (step.op.get(), step.gas_refund_counter))
            .collect::<Vec<_>>()
    };
    // A step reads the counter when it starts, so the `SSTORE` excludes its own refund, and a
    // call step excludes the refunds of its child.
    assert_eq!(
        steps(0),
        [
            (op::PUSH1, 0),
            (op::PUSH1, 0),
            (op::SSTORE, 0),
            (op::PUSH0, 4800),
            (op::PUSH0, 4800),
            (op::PUSH0, 4800),
            (op::PUSH0, 4800),
            (op::PUSH20, 4800),
            (op::GAS, 4800),
            (op::DELEGATECALL, 4800),
            (op::STOP, 2800),
        ]
    );
    assert_eq!(
        steps(1),
        [(op::PUSH1, 4800), (op::PUSH1, 4800), (op::SSTORE, 4800), (op::STOP, 2800)]
    );
}

#[test]
fn step_refund_counter_drops_reverted_child() {
    let caller = address!("1000000000000000000000000000000000000001");
    let parent = address!("2000000000000000000000000000000000000002");
    let child = address!("3000000000000000000000000000000000000003");

    // The child clears the parent's slot, which refunds 4800, and reverts, which discards it.
    let mut parent_code = vec![op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH20];
    parent_code.extend(child.as_slice());
    parent_code.extend([op::GAS, op::DELEGATECALL, op::STOP]);
    let child_code = [op::PUSH0, op::PUSH0, op::SSTORE, op::PUSH0, op::PUSH0, op::REVERT];

    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        &parent,
        AccountInfo { code: Some(Bytecode::new_raw(parent_code.into())), ..Default::default() },
    );
    db.insert_account_storage(&parent, &U256::ZERO, &U256::ONE);
    db.insert_account_info(
        &child,
        AccountInfo {
            code: Some(Bytecode::new_raw(Bytes::copy_from_slice(&child_code))),
            ..Default::default()
        },
    );
    let mut evm = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
        .build_mainnet_with_inspector(
            TracingInspector::new(TracingInspectorConfig::default_geth()),
        );
    evm.inspect_tx(TxEnv {
        caller,
        gas_limit: 1_000_000,
        kind: TransactTo::Call(parent),
        ..Default::default()
    })
    .unwrap();

    let refunds = |index: usize| {
        evm.inspector.traces().nodes()[index]
            .trace
            .steps
            .iter()
            .map(|step| step.gas_refund_counter)
            .collect::<Vec<_>>()
    };
    assert_eq!(refunds(0), [0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(refunds(1), [0, 0, 0, 4800, 4800, 4800]);
}
