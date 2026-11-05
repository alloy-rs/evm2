//! A transaction that runs several top-level calls (an account-abstraction batch) traces every one
//! of them: the recorded root moves under a synthetic root that the later calls join.

use crate::utils::{AccountInfo, Bytecode, SpecId};
use alloy_primitives::{Address, Bytes, TxKind, U256, hex};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::{CallConfig, FlatCallConfig};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, Version,
    ethereum::{ethereum_tx_registry, execute_initial_frame, prepare_initial_frame},
    evm::InMemoryDB,
    interpreter::GasTracker,
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig, types::CallKind};

const CALLER: Address = Address::repeat_byte(0xaa);
const OUTER: Address = Address::with_last_byte(0x42);
const INNER: Address = Address::with_last_byte(0x43);

/// Runs each of `calls` as a top-level call of one transaction under `tracer`.
fn run_batch(tracer: &mut TracingInspector, calls: &[(TxKind, Bytes)]) {
    let mut db = InMemoryDB::default();
    // `OUTER` calls `INNER`, which stops.
    db.insert_account_info(
        &OUTER,
        AccountInfo::default().with_code(Bytecode::new_raw(hex!("5f5f5f5f5f60435af100").into())),
    );
    db.insert_account_info(
        &INNER,
        AccountInfo::default().with_code(Bytecode::new_raw(hex!("00").into())),
    );
    let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::CANCUN, Version::new(SpecId::CANCUN)),
        SpecId::CANCUN,
        Default::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        Precompiles::base(SpecId::CANCUN),
    );
    evm.set_inspector(tracer);
    for (to, input) in calls {
        let mut gas = GasTracker::new(100_000);
        let frame =
            prepare_initial_frame(&mut evm, CALLER, 0, *to, input, U256::ZERO, &mut gas).unwrap();
        let result = execute_initial_frame(&mut evm, frame, &mut gas, 100_000, 0).unwrap();
        assert!(result.is_success(), "{:?}", result.stop);
    }
}

#[test]
fn batch_calls_share_a_synthetic_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    run_batch(
        &mut tracer,
        &[(TxKind::Call(OUTER), Bytes::new()), (TxKind::Create, Bytes::from_static(&hex!("00")))],
    );

    let nodes = tracer.traces().nodes();
    let shape: Vec<_> =
        nodes.iter().map(|node| (node.parent, node.trace.depth, node.children.clone())).collect();
    assert_eq!(
        shape,
        [(None, 0, vec![1, 3]), (Some(0), 1, vec![2]), (Some(1), 2, vec![]), (Some(0), 1, vec![])]
    );
    let root = &nodes[0].trace;
    assert_eq!((root.kind, root.caller, root.address), (CallKind::Call, CALLER, Address::ZERO));
    assert!(root.success);
    assert_eq!(root.gas_used, nodes[1].trace.gas_used + nodes[3].trace.gas_used);
    assert_eq!([nodes[1].trace.address, nodes[2].trace.address], [OUTER, INNER]);
    assert_eq!((nodes[3].trace.kind, nodes[3].trace.address), (CallKind::Create, CALLER.create(0)));

    let frame = tracer.geth_builder().geth_call_traces(CallConfig::default(), root.gas_used);
    let calls: Vec<_> =
        frame.calls.iter().map(|call| (call.typ.as_str(), call.to, call.calls.len())).collect();
    assert_eq!(calls, [("CALL", Some(OUTER), 1), ("CREATE", Some(CALLER.create(0)), 0)]);
    // Both calls' opcodes run one level under the root.
    let logs = tracer.geth_builder().geth_traces(0, Bytes::new(), Default::default()).struct_logs;
    let depths = (logs.first().map(|log| log.depth), logs.iter().map(|log| log.depth).max());
    assert_eq!(depths, (Some(2), Some(3)));
    let addresses: Vec<_> = tracer
        .clone()
        .into_parity_builder()
        .into_localized_transaction_traces(TransactionInfo::default())
        .into_iter()
        .map(|trace| trace.trace.trace_address)
        .collect();
    assert_eq!(addresses, [vec![], vec![0], vec![0, 0], vec![1]]);

    // A single call keeps the root to itself.
    tracer.fuse();
    run_batch(&mut tracer, &[(TxKind::Call(OUTER), Bytes::new())]);
    let nodes = tracer.traces().nodes();
    assert_eq!(nodes.len(), 2);
    assert_eq!((nodes[0].parent, nodes[0].trace.address, nodes[1].parent), (None, OUTER, Some(0)));
}

#[test]
fn every_top_level_call_is_a_child_of_one_root() {
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_geth());
    let call = (TxKind::Call(OUTER), Bytes::new());
    run_batch(&mut tracer, &[call.clone(), call.clone(), call]);

    let nodes = tracer.traces().nodes();
    let calls: Vec<_> = nodes[0]
        .children
        .iter()
        .map(|&child| (nodes[child].trace.depth, nodes[child].trace.address))
        .collect();
    assert_eq!(calls, [(1, OUTER); 3]);
}

#[test]
fn parity_traces_keep_a_batch_of_precompile_calls() {
    // Zero-value calls to the identity precompile, which Parity-style traces hide below the root.
    let call = (TxKind::Call(Address::with_last_byte(4)), Bytes::new());
    let mut tracer = TracingInspector::new(TracingInspectorConfig::default_parity());
    run_batch(&mut tracer, &[call.clone(), call]);

    let parity = tracer.clone().into_parity_builder();
    let flat = tracer.into_flat_call_builder(&FlatCallConfig::default());
    for builder in [parity, flat] {
        let traces = builder.into_localized_transaction_traces(TransactionInfo::default());
        let addresses: Vec<_> = traces.into_iter().map(|trace| trace.trace.trace_address).collect();
        assert_eq!(addresses, [vec![], vec![0], vec![1]]);
    }
}

#[test]
fn vm_trace_of_a_batch_is_empty() {
    // vmTrace hangs a call's subtrace on the instruction that made it, and a batch root runs none.
    let call = (TxKind::Call(OUTER), Bytes::new());
    let mut tracer = TracingInspector::new(TracingInspectorConfig::parity_vm_trace());
    run_batch(&mut tracer, std::slice::from_ref(&call));
    assert!(!tracer.clone().into_parity_builder().vm_trace().ops.is_empty());
    tracer.fuse();
    run_batch(&mut tracer, &[call.clone(), call]);
    assert!(tracer.into_parity_builder().vm_trace().ops.is_empty());
}
