//! Recorded refund counters must never wrap when the counter goes negative.

use crate::utils::{AccountInfo, Bytecode, CacheDB, EmptyDB, SpecId, TransactTo, TxEnv};
use alloy_primitives::{Address, U256, hex};
use evm2::{BaseEvmTypes, Evm, Precompiles, ethereum::ethereum_tx_registry};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

/// Runs `code` against a child at `0x43`, with the target's storage slot 0 preset to `1`.
///
/// The preset slot is what allows a nested frame to drive its refund counter below zero:
/// clearing a slot whose original value is non-zero credits the clearing refund, and
/// rewriting that slot with a non-zero value while its original value is *still* non-zero
/// debits the same refund. Only a frame that inherits the cleared slot — a child sharing
/// the parent's storage, as `DELEGATECALL` provides — observes the debit on its own
/// counter, because within a single frame the credit and the debit cancel out.
fn inspect(code: &[u8], child: &[u8]) -> TracingInspector {
    let target = Address::with_last_byte(0x42);
    let mut db = CacheDB::<EmptyDB>::default();
    for (address, code) in [(target, code), (Address::with_last_byte(0x43), child)] {
        db.insert_account_info(
            &address,
            AccountInfo::default().with_code(Bytecode::new_raw(code.to_vec().into())),
        );
    }
    db.insert_account_storage(&target, &U256::ZERO, &U256::from(1));

    let spec = SpecId::CANCUN;
    let mut inspector = TracingInspector::new(TracingInspectorConfig::none().set_steps(true));
    let mut evm = Evm::<BaseEvmTypes>::new(
        spec,
        Default::default(),
        ethereum_tx_registry(spec),
        db,
        Precompiles::base(spec),
    );
    evm.set_inspector(&mut inspector);
    let tx = TxEnv::builder().gas_limit(1_000_000).kind(TransactTo::Call(target)).build_fill();
    assert!(evm.transact(&tx.envelope()).is_ok());
    drop(evm);

    inspector
}

#[test]
fn step_refund_counter_is_clamped_when_negative() {
    // `PUSH0 PUSH0 SSTORE` clears slot 0, then `DELEGATECALL` runs `PUSH1 2 PUSH1 0 SSTORE`
    // in the caller's storage. The delegatecall frame sees original = 1, present = 0,
    // new = 2, which debits the clearing refund from its own counter.
    let parent = hex!("5f5f555f5f5f5f604361fffff400");
    let child = hex!("600260005500");

    let inspector = inspect(&parent, &child);
    let nodes = inspector.traces().nodes();
    assert_eq!(nodes.len(), 2, "{nodes:#?}");
    assert!(nodes.iter().all(|node| node.trace.success));

    // The delegatecall frame's own counter goes negative on its final step and must be
    // reported as zero rather than as a `u64` wrap of the negative `i64`.
    let child_steps = &nodes[1].trace.steps;
    assert_eq!(
        child_steps.iter().map(|step| step.gas_refund_counter).collect::<Vec<_>>(),
        vec![0, 0, 0, 0],
        "the delegatecall frame's negative refund counter must be clamped"
    );

    // The parent's clearing credit is the only non-zero counter recorded anywhere, so a
    // wrap anywhere else shows up as an implausibly large maximum.
    let max = nodes
        .iter()
        .flat_map(|node| &node.trace.steps)
        .map(|step| step.gas_refund_counter)
        .max()
        .unwrap();
    assert_eq!(max, 4_800, "a negative refund counter wrapped to `u64`");
}
