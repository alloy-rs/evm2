//! Transactions that run several top-level calls, such as account-abstraction batches.

use alloy_primitives::{Address, Bytes, TxKind, U256, hex};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::CallConfig;
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    ethereum::{ethereum_tx_registry, execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{GasTracker, InstrStop},
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig, types::CallKind};

const CALLER: Address = Address::repeat_byte(0xaa);
/// Calls `INNER` with no input, then stops.
const OUTER: Address = Address::with_last_byte(0x42);
/// Stops.
const INNER: Address = Address::with_last_byte(0x43);
/// Reverts.
const REVERTER: Address = Address::with_last_byte(0x44);

/// Runs each of `calls` as a top-level call of one transaction under `inspector`, returning
/// whether each call succeeded.
fn run_calls<I: evm2::Inspector<BaseEvmTypes>>(inspector: &mut I, calls: &[Address]) -> Vec<bool> {
    let mut db = InMemoryDB::default();
    let code = |code: &[u8]| {
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::copy_from_slice(code)))
    };
    db.insert_account_info(&OUTER, code(&hex!("5f5f5f5f5f60435af100")));
    db.insert_account_info(&INNER, code(&hex!("00")));
    db.insert_account_info(&REVERTER, code(&hex!("5f5ffd")));
    let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::CANCUN, Version::new(SpecId::CANCUN)),
        SpecId::CANCUN,
        Default::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        Precompiles::base(SpecId::CANCUN),
    );
    evm.set_inspector(inspector);
    calls
        .iter()
        .map(|&to| {
            let mut gas = GasTracker::new(100_000);
            let frame = prepare_initial_frame(
                &mut evm,
                CALLER,
                0,
                TxKind::Call(to),
                &Bytes::new(),
                U256::ZERO,
                &mut gas,
            )
            .unwrap();
            execute_initial_frame(&mut evm, frame, &mut gas, 100_000, 0).unwrap().is_success()
        })
        .collect()
}

#[test]
fn batch_calls_share_a_synthetic_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    assert_eq!(run_calls(&mut tracer, &[OUTER, INNER]), [true, true]);

    let nodes = tracer.traces().nodes();
    let shape: Vec<_> = nodes
        .iter()
        .map(|node| (node.parent, node.trace.depth, node.trace.address, node.children.clone()))
        .collect();
    assert_eq!(
        shape,
        [
            (None, 0, Address::ZERO, vec![1, 3]),
            (Some(0), 1, OUTER, vec![2]),
            (Some(1), 2, INNER, vec![]),
            (Some(0), 1, INNER, vec![]),
        ]
    );
    let root = &nodes[0].trace;
    assert_eq!((root.kind, root.caller, root.success), (CallKind::Call, CALLER, true));
    assert_eq!(root.gas_used, nodes[1].trace.gas_used + nodes[3].trace.gas_used);

    let frame = tracer.geth_builder().geth_call_traces(CallConfig::default(), root.gas_used);
    let calls: Vec<_> = frame.calls.iter().map(|call| (call.to, call.calls.len())).collect();
    assert_eq!((frame.to, calls), (Some(Address::ZERO), vec![(Some(OUTER), 1), (Some(INNER), 0)]));

    // The batch's calls sit one level below the synthetic root, and so do their opcodes.
    let logs = tracer.geth_builder().geth_traces(0, Bytes::new(), Default::default()).struct_logs;
    let depths: Vec<_> = logs.iter().map(|log| log.depth).collect();
    assert_eq!(depths, [2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 2]);

    let addresses: Vec<_> = tracer
        .into_parity_builder()
        .into_localized_transaction_traces(TransactionInfo::default())
        .into_iter()
        .map(|trace| trace.trace.trace_address)
        .collect();
    assert_eq!(addresses, [vec![], vec![0], vec![0, 0], vec![1]]);
}

#[test]
fn single_top_level_call_is_the_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    run_calls(&mut tracer, &[OUTER]);
    let nodes = tracer.traces().nodes();
    let shape: Vec<_> = nodes.iter().map(|node| (node.parent, node.trace.address)).collect();
    assert_eq!(shape, [(None, OUTER), (Some(0), INNER)]);
}

#[test]
fn failed_batch_call_fails_the_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    assert_eq!(run_calls(&mut tracer, &[INNER, REVERTER]), [true, false]);
    let root = &tracer.traces().nodes()[0].trace;
    assert_eq!((root.success, root.status), (false, Some(InstrStop::Revert)));
}

#[test]
fn vm_trace_of_a_batch_running_different_code() {
    // The root used to keep the first call's step ordering and index the last call's steps.
    let mut tracer =
        TracingInspector::new(TracingInspectorConfig::default_parity().set_steps(true));
    run_calls(&mut tracer, &[OUTER, REVERTER]);
    assert!(tracer.into_parity_builder().vm_trace().ops.is_empty());
}
