//! Transactions that run several top-level calls, such as account-abstraction batches.

use crate::utils::TxEnv;
use alloy_primitives::{Address, Bytes, TxKind, U256, hex};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::{
    CallConfig,
    erc7562::{Erc7562Config, Erc7562Frame},
};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, TxResultExt, Version,
    bytecode::Bytecode,
    ethereum::{ethereum_tx_registry, execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{GasTracker, InstrStop, op},
};
use evm2_inspectors::tracing::{
    Erc7562Inspector, TracingInspector, TracingInspectorConfig, types::CallKind,
};

const CALLER: Address = Address::repeat_byte(0xaa);
/// Calls `INNER` with no input, then stops.
const OUTER: Address = Address::with_last_byte(0x42);
/// Stops.
const INNER: Address = Address::with_last_byte(0x43);
/// Reverts.
const REVERTER: Address = Address::with_last_byte(0x44);
/// Destroys itself.
const DESTRUCTOR: Address = Address::with_last_byte(0x45);

/// Runs each of `calls` as a top-level call of one transaction under `inspector`, returning
/// whether each call succeeded. With `batch_start`, the inspector is told first that the calls
/// form a batch.
fn run_calls<I: evm2::Inspector<BaseEvmTypes>>(
    inspector: &mut I,
    calls: &[Address],
    batch_start: bool,
) -> Vec<bool> {
    let mut db = InMemoryDB::default();
    let code = |code: &[u8]| {
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::copy_from_slice(code)))
    };
    db.insert_account_info(&OUTER, code(&hex!("5f5f5f5f5f60435af100")));
    db.insert_account_info(&INNER, code(&hex!("00")));
    db.insert_account_info(&REVERTER, code(&hex!("5f5ffd")));
    db.insert_account_info(&DESTRUCTOR, code(&hex!("5fff")));
    let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::CANCUN, Version::new(SpecId::CANCUN)),
        SpecId::CANCUN,
        Default::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        Precompiles::base(SpecId::CANCUN),
    );
    evm.set_inspector(inspector);
    if batch_start {
        evm.inspect_batch_start();
    }
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
    assert_eq!(run_calls(&mut tracer, &[OUTER, INNER], false), [true, true]);

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

    // The opcodes keep the depth the EVM runs them at.
    let logs = tracer.geth_builder().geth_traces(0, Bytes::new(), Default::default()).struct_logs;
    let depths: Vec<_> = logs.iter().map(|log| log.depth).collect();
    assert_eq!(depths, [1, 1, 1, 1, 1, 1, 1, 1, 2, 1, 1]);

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
    run_calls(&mut tracer, &[OUTER], false);
    let nodes = tracer.traces().nodes();
    let shape: Vec<_> = nodes.iter().map(|node| (node.parent, node.trace.address)).collect();
    assert_eq!(shape, [(None, OUTER), (Some(0), INNER)]);
}

#[test]
fn failed_batch_call_fails_the_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    assert_eq!(run_calls(&mut tracer, &[INNER, REVERTER], false), [true, false]);
    let root = &tracer.traces().nodes()[0].trace;
    assert_eq!((root.success, root.status), (false, Some(InstrStop::Revert)));
}

#[test]
fn vm_trace_of_a_batch_running_different_code() {
    // The root used to keep the first call's step ordering and index the last call's steps.
    let mut tracer =
        TracingInspector::new(TracingInspectorConfig::default_parity().set_steps(true));
    run_calls(&mut tracer, &[OUTER, REVERTER], false);
    let trace = tracer.into_parity_builder().vm_trace();
    let calls: Vec<_> = trace
        .ops
        .iter()
        .map(|entry| (entry.op.clone(), entry.sub.as_ref().map(|sub| sub.ops.len())))
        .collect();
    assert_eq!(calls, [(None, Some(9)), (None, Some(3))]);
}

#[test]
fn batch_root_does_not_destroy_itself() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_parity());
    assert_eq!(run_calls(&mut tracer, &[INNER, DESTRUCTOR], true), [true, true]);
    let nodes = tracer.traces().nodes();
    assert_eq!((nodes[0].trace.status, nodes[0].is_selfdestruct()), (Some(InstrStop::Stop), false));
    assert!(nodes[2].is_selfdestruct());
}

#[test]
fn erc7562_counters_follow_a_nested_root() {
    let mut inspector = Erc7562Inspector::new(Erc7562Config::default());
    run_calls(&mut inspector, &[OUTER, INNER], false);
    let tx = TxEnv { kind: TxKind::Call(OUTER), ..Default::default() }.envelope();
    let root = inspector.frame(&tx, &TxResultExt::<()>::default()).unwrap();
    let opcodes = |frame: &Erc7562Frame| {
        let mut opcodes: Vec<_> = frame.used_opcodes.keys().copied().collect();
        opcodes.sort_unstable();
        opcodes
    };
    assert!(root.used_opcodes.is_empty());
    assert_eq!(opcodes(&root.calls[0]), [op::STOP, op::CALL]);
    assert_eq!(opcodes(&root.calls[0].calls[0]), [op::STOP]);
    assert_eq!(opcodes(&root.calls[1]), [op::STOP]);
}

#[test]
fn started_batch_with_one_call_has_a_synthetic_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    assert_eq!(run_calls(&mut tracer, &[OUTER], true), [true]);

    let nodes = tracer.traces().nodes();
    let shape: Vec<_> = nodes
        .iter()
        .map(|node| (node.parent, node.trace.depth, node.trace.address, node.children.clone()))
        .collect();
    assert_eq!(
        shape,
        [
            (None, 0, Address::ZERO, vec![1]),
            (Some(0), 1, OUTER, vec![2]),
            (Some(1), 2, INNER, vec![])
        ]
    );
    let (root, call) = (&nodes[0].trace, &nodes[1].trace);
    assert_eq!((root.caller, root.success), (CALLER, true));
    assert_eq!((root.gas_limit, root.gas_used), (call.gas_limit, call.gas_used));

    let frame = tracer.geth_builder().geth_call_traces(CallConfig::default(), root.gas_used);
    let calls: Vec<_> = frame.calls.iter().map(|call| call.to).collect();
    assert_eq!((frame.to, calls), (Some(Address::ZERO), vec![Some(OUTER)]));
}

#[test]
fn started_batch_records_the_same_trace_as_a_nested_one() {
    let mut started = TracingInspector::new(TracingInspectorConfig::default_geth());
    run_calls(&mut started, &[OUTER, INNER], true);
    let mut nested = TracingInspector::new(TracingInspectorConfig::default_geth());
    run_calls(&mut nested, &[OUTER, INNER], false);
    assert_eq!(started.traces().nodes(), nested.traces().nodes());
}
