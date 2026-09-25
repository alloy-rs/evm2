//! Recording inputs is optional and does not affect execution.

use crate::utils::{AccountInfo, Bytecode, CacheDB, EmptyDB, SpecId, TransactTo, TxEnv};
use alloy_primitives::{Address, Bytes, hex};
use evm2::{BaseEvmTypes, Evm, Precompiles, ethereum::ethereum_tx_registry};
use evm2_inspectors::tracing::{
    TraceLimitBehavior, TraceLimits, TracingInspector, TracingInspectorConfig,
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
