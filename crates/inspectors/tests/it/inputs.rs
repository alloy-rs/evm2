//! Recording inputs is optional and does not affect execution.

use crate::utils::{AccountInfo, Bytecode, CacheDB, EmptyDB, SpecId, TransactTo, TxEnv};
use alloy_primitives::{Address, Bytes, U256, hex};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Inspector, Precompiles, Version,
    constants::CALL_DEPTH_LIMIT, ethereum::ethereum_tx_registry,
};
use evm2_inspectors::{
    access_list::AccessListInspector,
    opcode::OpcodeGasInspector,
    otterscan::InternalOperationsInspector,
    tracing::{TraceLimitBehavior, TraceLimits, TracingInspector, TracingInspectorConfig},
    transfer::TransferInspector,
};

/// A single `CALL` passing 64 KiB of memory as calldata, which evm2 copies into a
/// separate input buffer for the child message.
///
/// `PUSH0 PUSH0 PUSH3 0x010000 PUSH0 PUSH0 PUSH1 0x43 PUSH2 0xffff CALL STOP`
const ONE_BIG_CALL: &[u8] = &hex!("5f5f620100005f5f604361fffff100");
const TWO_BIG_CALLS: &[u8] = &hex!("5f5f620100005f5f604361fffff15f5f620100005f5f604361fffff100");
const BIG_CALL_INPUT_LEN: usize = 65536;

/// Runs `code` with the given config, calling into a `STOP`-only child at `0x43`.
fn inspect(code: &[u8], config: TracingInspectorConfig) -> TracingInspector {
    inspect_with_data(code, Bytes::new(), config)
}

/// As [`inspect`], with `data` as the transaction calldata.
fn inspect_with_data(code: &[u8], data: Bytes, config: TracingInspectorConfig) -> TracingInspector {
    let (inspector, completed) = inspect_with_limits(code, data, config, TraceLimits::default());
    assert!(completed);
    inspector
}

fn inspect_with_limits(
    code: &[u8],
    data: Bytes,
    config: TracingInspectorConfig,
    limits: TraceLimits,
) -> (TracingInspector, bool) {
    let target = Address::with_last_byte(0x42);
    let mut db = CacheDB::<EmptyDB>::default();
    for (address, code) in [(target, code), (Address::with_last_byte(0x43), &hex!("00")[..])] {
        db.insert_account_info(
            &address,
            AccountInfo::default().with_code(Bytecode::new_raw(code.to_vec().into())),
        );
    }
    let mut inspector = TracingInspector::new(config).with_limits(limits);
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::PRAGUE,
        Default::default(),
        ethereum_tx_registry(SpecId::PRAGUE),
        db,
        Precompiles::base(SpecId::PRAGUE),
    );
    evm.set_inspector(&mut inspector);
    let tx = TxEnv::builder()
        .gas_limit(1_000_000)
        .kind(TransactTo::Call(target))
        .data(data)
        .build_fill();
    let completed = evm.transact(&tx.envelope()).is_ok();
    drop(evm);
    (inspector, completed)
}

#[test]
fn child_and_transaction_inputs_are_optional() {
    for record_inputs in [false, true] {
        let inspector = inspect_with_data(
            ONE_BIG_CALL,
            Bytes::from_static(&[1, 2, 3, 4]),
            TracingInspectorConfig::none().set_record_inputs(record_inputs),
        );
        let nodes = inspector.traces().nodes();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|node| node.trace.success));
        assert_eq!(nodes[0].trace.data.len(), if record_inputs { 4 } else { 0 });
        assert_eq!(nodes[1].trace.data.len(), if record_inputs { BIG_CALL_INPUT_LEN } else { 0 });
    }
}

#[test]
fn creation_input_is_optional() {
    // Store init code that returns one byte of runtime code, then CREATE with those 10 bytes.
    let code = hex!("6960fe5f5360015ff300005f52600a60165ff000");
    for record_inputs in [false, true] {
        let inspector =
            inspect(&code, TracingInspectorConfig::none().set_record_inputs(record_inputs));
        let nodes = inspector.traces().nodes();
        assert_eq!(nodes.len(), 2);
        let trace = &nodes[1].trace;
        assert!(trace.kind.is_any_create());
        assert!(trace.success);
        assert_eq!(trace.output.as_ref(), &[0xfe]);
        assert_eq!(trace.data.len(), if record_inputs { 10 } else { 0 });
    }
}

#[test]
fn disabling_recording_preserves_calldata_execution() {
    // Return the first calldata word.
    let code = hex!("5f355f5260205ff3");
    let data = Bytes::from(vec![7; 32]);
    for record_inputs in [false, true] {
        let inspector = inspect_with_data(
            &code,
            data.clone(),
            TracingInspectorConfig::none().set_record_inputs(record_inputs),
        );
        let trace = &inspector.traces().nodes()[0].trace;
        assert!(trace.success);
        assert_eq!(trace.output, data);
        assert_eq!(trace.data.len(), if record_inputs { 32 } else { 0 });
    }
}

#[test]
fn byte_budget_skips_child_input_and_preserves_execution() {
    let config = TracingInspectorConfig::none().set_record_inputs(true);
    for limit in [BIG_CALL_INPUT_LEN - 1, BIG_CALL_INPUT_LEN] {
        let (mut inspector, completed) = inspect_with_limits(
            ONE_BIG_CALL,
            Bytes::new(),
            config,
            TraceLimits::default().set_max_recorded_bytes(Some(limit)),
        );
        assert!(completed);
        assert_eq!(inspector.recorded_bytes(), if limit == BIG_CALL_INPUT_LEN { limit } else { 0 });
        assert_eq!(inspector.limit_exceeded(), limit < BIG_CALL_INPUT_LEN);
        assert_eq!(
            inspector.traces().nodes()[1].trace.data.len(),
            if limit == BIG_CALL_INPUT_LEN { BIG_CALL_INPUT_LEN } else { 0 },
        );
        inspector.fuse();
        assert_eq!(inspector.recorded_bytes(), 0);
        assert!(!inspector.limit_exceeded());
    }
}

#[test]
fn byte_budget_can_halt_execution() {
    let (inspector, completed) = inspect_with_limits(
        ONE_BIG_CALL,
        Bytes::new(),
        TracingInspectorConfig::none().set_record_inputs(true),
        TraceLimits::default()
            .set_max_recorded_bytes(Some(BIG_CALL_INPUT_LEN - 1))
            .set_behavior(TraceLimitBehavior::Halt),
    );
    assert!(!completed);
    assert!(inspector.limit_exceeded());
    assert_eq!(inspector.recorded_bytes(), 0);
}

#[test]
fn byte_budget_stops_retaining_later_child_inputs() {
    let (inspector, completed) = inspect_with_limits(
        TWO_BIG_CALLS,
        Bytes::new(),
        TracingInspectorConfig::none().set_record_inputs(true),
        TraceLimits::default().set_max_recorded_bytes(Some(BIG_CALL_INPUT_LEN)),
    );
    assert!(completed);
    assert!(inspector.limit_exceeded());
    assert_eq!(inspector.recorded_bytes(), BIG_CALL_INPUT_LEN);
    let nodes = inspector.traces().nodes();
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[1].trace.data.len(), BIG_CALL_INPUT_LEN);
    assert!(nodes[2].trace.data.is_empty());
}

#[test]
fn byte_budget_counts_memory_snapshots() {
    // MSTORE expands memory to 32 bytes, so the next step takes a 32-byte snapshot.
    let code = hex!("60015f5200");
    let config = TracingInspectorConfig::none().steps().memory_snapshots();
    let (inspector, completed) = inspect_with_limits(
        &code,
        Bytes::new(),
        config,
        TraceLimits::default().set_max_recorded_bytes(Some(31)),
    );
    assert!(completed);
    assert!(inspector.limit_exceeded());
    assert_eq!(inspector.recorded_bytes(), 0);
}

#[test]
fn byte_budget_covers_immediates_and_memory_deltas() {
    let code = hex!("60015f5200");
    for (config, limit) in [
        (TracingInspectorConfig::none().steps().set_immediate_bytes(true), 0),
        (TracingInspectorConfig::none().steps().set_step_deltas(true), 31),
    ] {
        let (inspector, completed) = inspect_with_limits(
            &code,
            Bytes::new(),
            config,
            TraceLimits::default().set_max_recorded_bytes(Some(limit)),
        );
        assert!(completed);
        assert!(inspector.limit_exceeded());
        assert_eq!(inspector.recorded_bytes(), 0);
    }
}

#[test]
fn byte_budget_can_halt_from_a_step() {
    let (inspector, completed) = inspect_with_limits(
        &hex!("600100"),
        Bytes::new(),
        TracingInspectorConfig::none().steps().set_immediate_bytes(true),
        TraceLimits::default()
            .set_max_recorded_bytes(Some(0))
            .set_behavior(TraceLimitBehavior::Halt),
    );
    assert!(!completed);
    assert!(inspector.limit_exceeded());
}

#[test]
fn nested_roots_preserve_trace_depth_and_top_level_inspector_semantics() {
    let caller = Address::repeat_byte(0xaa);
    let child = Address::with_last_byte(0x43);
    // Send 1 wei to a child that executes STOP.
    let code = Bytes::from_static(&hex!("5f5f5f5f600160435af100"));
    for initial_call_depth in [0, 7, u16::MAX - CALL_DEPTH_LIMIT - 1] {
        for kind in [TransactTo::Call(Address::with_last_byte(0x42)), TransactTo::Create] {
            let target = kind.to().copied().unwrap_or_else(|| caller.create(0));
            let data = if kind.is_create() { code.clone() } else { Bytes::from_static(&[1, 2]) };
            let run = |inspector: &mut dyn Inspector<BaseEvmTypes>| {
                let mut db = CacheDB::<EmptyDB>::default();
                db.insert_account_info(
                    &caller,
                    AccountInfo::default().with_balance(U256::from(100)),
                );
                db.insert_account_info(
                    &child,
                    AccountInfo::default().with_code(Bytecode::new_raw(hex!("00").into())),
                );
                if !kind.is_create() {
                    db.insert_account_info(
                        &target,
                        AccountInfo::default().with_code(Bytecode::new_raw(code.clone())),
                    );
                }
                let mut version = Version::new(SpecId::CANCUN);
                version.initial_call_depth = initial_call_depth;
                let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
                    ExecutionConfig::for_spec_and_version(SpecId::CANCUN, version),
                    SpecId::CANCUN,
                    Default::default(),
                    ethereum_tx_registry(SpecId::CANCUN),
                    db,
                    Precompiles::base(SpecId::CANCUN),
                );
                evm.set_inspector(inspector);
                let tx = TxEnv {
                    caller,
                    kind,
                    value: U256::from(7),
                    gas_limit: 100_000,
                    data: data.clone(),
                    ..Default::default()
                };
                assert!(evm.transact(&tx.envelope()).unwrap().result().status);
            };
            let mut tracer = TracingInspector::new(
                TracingInspectorConfig::default_geth().set_record_inputs(true),
            )
            .with_limits(TraceLimits::default().set_max_recorded_bytes(Some(0)));
            run(&mut tracer);
            let nodes = tracer.traces().nodes();
            assert_eq!(nodes.len(), 2);
            assert_eq!(nodes[0].parent, None);
            assert_eq!(nodes[0].trace.depth, usize::from(initial_call_depth));
            assert_eq!(nodes[0].trace.data, data);
            assert_eq!(nodes[0].children, [1]);
            assert_eq!(nodes[1].parent, Some(0));
            assert_eq!(nodes[1].trace.depth, usize::from(initial_call_depth) + 1);
            let geth = tracer.geth_builder().geth_traces(0, Bytes::new(), Default::default());
            assert_eq!(geth.struct_logs[0].depth, u64::from(initial_call_depth) + 1);
            assert!(
                geth.struct_logs.iter().any(|log| log.depth == u64::from(initial_call_depth) + 2)
            );

            let mut access = AccessListInspector::default();
            run(&mut access);
            assert!(access.excluded().contains(&caller));
            assert!(access.excluded().contains(&target));
            assert!(access.touched_slots().contains_key(&child));
            let mut operations = InternalOperationsInspector::default();
            run(&mut operations);
            assert_eq!(operations.operations().len(), 1);
            assert_eq!(operations.operations()[0].from, target);
            assert_eq!(operations.operations()[0].to, child);
            let mut transfers = TransferInspector::internal_only();
            run(&mut transfers);
            assert_eq!(transfers.transfers().len(), 1);
            assert_eq!(transfers.transfers()[0].from, target);
            assert_eq!(transfers.transfers()[0].to, child);
            let mut opcodes = OpcodeGasInspector::default();
            run(&mut opcodes);
            assert!(
                opcodes
                    .opcode_iter()
                    .any(|(op, (count, gas))| op.get() == 0xf1 && count == 1 && gas > 0)
            );
        }
    }
}
