//! Cooperative cancellation at EVM and JavaScript callback boundaries.

use super::*;
use alloy_consensus::TxLegacy;
use alloy_primitives::hex;
use boa_engine::NativeFunction;
use boa_gc::{Finalize, Trace};
use evm2::{
    BaseEvmTypes, Precompiles, SpecId,
    bytecode::Bytecode,
    env::{BlockEnvExt, TxEnv},
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, CacheDB, EmptyDB},
    registry::HandlerError,
};

const TRACER: &str = "{ fault: function() {}, result: function() { return 42; } }";
const TARGET: Address = Address::with_last_byte(0x42);

#[test]
fn unset_interrupt_preserves_results() {
    for interrupt in [None, Some(JsInspectorInterrupt::default())] {
        let mut inspector = JsInspector::new(TRACER.into(), serde_json::Value::Null).unwrap();
        if let Some(interrupt) = interrupt {
            inspector = inspector.with_interrupt(interrupt);
        }
        let (mut inspector, result) = run(inspector, &hex!("60015000"), TxKind::Call(TARGET));
        let result = result.unwrap();
        assert!(result.result.status);
        assert_eq!(
            inspector
                .json_result(
                    &result,
                    &transaction(TxKind::Call(TARGET)),
                    &BlockEnvExt::default(),
                    &mut EmptyDB::default()
                )
                .unwrap(),
            serde_json::json!(42)
        );
    }
}

#[test]
fn pre_cancelled_calls_and_creates_abort() {
    for kind in [
        TxKind::Call(TARGET),
        // Empty account and identity precompile have no interpreter steps.
        TxKind::Call(Address::with_last_byte(0x43)),
        TxKind::Call(Address::with_last_byte(4)),
        TxKind::Create,
    ] {
        let interrupt = JsInspectorInterrupt::new();
        interrupt.interrupt();
        let inspector = JsInspector::new(TRACER.into(), serde_json::Value::Null)
            .unwrap()
            .with_interrupt(interrupt);
        let (_, result) = run(inspector, &hex!("60015000"), kind);
        assert_interrupted(result);
    }
}

#[test]
fn interrupt_stops_steps_without_js_step_callback() {
    let interrupt = JsInspectorInterrupt::new();
    let mut inspector = JsInspector::new(TRACER.into(), serde_json::Value::Null)
        .unwrap()
        .with_interrupt(interrupt.clone());
    let tx_env = TxEnv::<BaseEvmTypes>::default();
    let message = Message::<BaseEvmTypes>::default();
    let mut interp = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);
    // Signal from another thread after the inspector has been created.
    std::thread::spawn(move || interrupt.interrupt()).join().unwrap();
    inspector.step(&mut interp);
    assert_eq!(interp.result(), Err(InstrStop::FatalExternalError));
}

#[test]
fn interrupt_during_step_aborts_execution() {
    for (code, op) in [
        (&hex!("60015000")[..], "PUSH1"),
        (&hex!("00")[..], "STOP"),
        (&hex!("5f5ff3")[..], "RETURN"),
        (&hex!("5f5ffd")[..], "REVERT"),
        (&hex!("5f5f5f5f5f600461fffff100")[..], "CALL"),
        (&hex!("5f5f5ff000")[..], "CREATE"),
    ] {
        let script = format!(
            "{{ step: function(log) {{ if (log.op.toString() === '{op}') interrupt(); }},
                fault: function() {{ interrupt(); }},
                result: function() {{ return 42; }} }}"
        );
        let inspector = interruptible(&script);
        let (_, result) = run(inspector, code, TxKind::Call(TARGET));
        assert_interrupted(result);
    }
}

#[test]
fn interrupt_during_enter_and_exit_aborts() {
    for callback in ["enter", "exit"] {
        for code in [
            // Identity precompile and empty initcode exercise frames without EVM steps.
            &hex!("5f5f5f5f5f600461fffff100")[..],
            &hex!("5f5f5ff000")[..],
            // SELFDESTRUCT invokes enter/exit outside of step_end.
            &hex!("6043ff")[..],
        ] {
            let script = format!(
                "{{ {callback}: function() {{ interrupt(); }},
                    fault: function() {{}}, result: function() {{ return 42; }} }}"
            );
            let (_, result) = run(interruptible(&script), code, TxKind::Call(TARGET));
            assert_interrupted(result);
        }
    }
}

#[test]
fn interrupt_before_step_end_skips_callback_and_preserves_error() {
    for preserve_error in [false, true] {
        let interrupt = JsInspectorInterrupt::new();
        let mut inspector = JsInspector::new(
            "{step: function() { throw 'must not run'; }, fault: function() {}, result: function() {}}"
                .into(),
            serde_json::Value::Null,
        )
        .unwrap()
        .with_interrupt(interrupt.clone());
        let tx_env = TxEnv::<BaseEvmTypes>::default();
        let message = Message::<BaseEvmTypes>::default();
        let mut interp = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);
        inspector.step(&mut interp);
        assert!(inspector.pending_steps[0].active);
        let error = if preserve_error {
            let error = ExecutionError::Fatal("original error".into());
            let stop = interp.fail(error.clone());
            interp.set_stop(stop);
            error
        } else {
            interp.set_stop(InstrStop::Stop);
            ExecutionError::Fatal("JavaScript tracing interrupted".into())
        };
        interrupt.interrupt();
        inspector.step_end(&mut interp);
        assert!(!inspector.pending_steps[0].active);
        assert_eq!(interp.result(), Err(InstrStop::FatalExternalError));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );
        assert_eq!(interp.run(&mut evm).unwrap_err().to_string(), error.to_string());
    }
}

#[test]
fn interrupt_survives_clone_and_fuse() {
    let interrupt = JsInspectorInterrupt::new();
    let mut inspector = JsInspector::new(TRACER.into(), serde_json::Value::Null)
        .unwrap()
        .with_interrupt(interrupt.clone());
    let cloned = inspector.try_clone().unwrap();
    inspector.fuse().unwrap();
    interrupt.interrupt();
    assert!(matches!(inspector.fuse(), Err(JsInspectorError::Interrupted)));
    assert!(matches!(inspector.try_clone(), Err(JsInspectorError::Interrupted)));
    for inspector in [inspector, cloned] {
        let (_, result) = run(inspector, &hex!("00"), TxKind::Call(TARGET));
        assert_interrupted(result);
    }
}

#[test]
fn interrupt_rejects_result_collection() {
    for script in [
        TRACER,
        "{fault: function() {}, result: function() { interrupt(); return 42; }}",
        "{fault: function() {}, result: function() { return {toJSON: function() { interrupt(); return 42; }}; }}",
    ] {
        let (mut inspector, result) = run(interruptible(script), &hex!("00"), TxKind::Call(TARGET));
        let result = result.unwrap();
        if script == TRACER {
            inspector.interrupt.as_ref().unwrap().interrupt();
        }
        assert!(matches!(
            inspector.json_result(
                &result,
                &transaction(TxKind::Call(TARGET)),
                &BlockEnvExt::default(),
                &mut EmptyDB::default()
            ),
            Err(JsInspectorError::Interrupted)
        ));
    }
}

#[test]
fn drop_guard_interrupts_when_request_future_is_dropped() {
    let interrupt = JsInspectorInterrupt::new();
    let inspector = JsInspector::new(TRACER.into(), serde_json::Value::Null)
        .unwrap()
        .with_interrupt(interrupt.clone());
    let guard = interrupt.drop_guard();
    let request = async move {
        let _guard = guard;
        core::future::pending::<()>().await;
    };
    // Ordinary handles can be dropped without interrupting the worker.
    drop(interrupt);
    assert!(!inspector.is_interrupted());
    // The guard also covers a request cancelled before its first poll.
    drop(request);
    let (_, result) = run(inspector, &hex!("60015000"), TxKind::Call(TARGET));
    assert_interrupted(result);
}

/// An atomic flag contains no garbage-collected JavaScript values.
#[derive(Trace, Finalize)]
struct InterruptSignal(#[unsafe_ignore_trace] JsInspectorInterrupt);

fn interruptible(script: &str) -> JsInspector {
    let interrupt = JsInspectorInterrupt::new();
    let mut inspector = JsInspector::new(script.into(), serde_json::Value::Null)
        .unwrap()
        .with_interrupt(interrupt.clone());
    inspector
        .ctx
        .register_global_builtin_callable(
            js_string!("interrupt"),
            0,
            NativeFunction::from_copy_closure_with_captures(
                |_, _, signal, _| {
                    signal.0.interrupt();
                    Ok(JsValue::undefined())
                },
                InterruptSignal(interrupt),
            ),
        )
        .unwrap();
    inspector
}

fn run(
    inspector: JsInspector,
    code: &[u8],
    kind: TxKind,
) -> (JsInspector, Result<TxResultWithState, HandlerError>) {
    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        &TARGET,
        AccountInfo::default().with_code(Bytecode::new_raw(code.to_vec().into())),
    );
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::CANCUN,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        Precompiles::base(SpecId::CANCUN),
    );
    evm.set_inspector(inspector);
    let result = evm.transact(&transaction(kind)).map(|result| result.detach());
    (*evm.clear_inspector_as::<JsInspector>().unwrap(), result)
}

fn assert_interrupted(result: Result<TxResultWithState, HandlerError>) {
    assert!(matches!(
        result,
        Err(HandlerError::Fatal(message)) if message.to_string() == "JavaScript tracing interrupted"
    ));
}

fn transaction(kind: TxKind) -> Recovered<TxEnvelope> {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy { gas_limit: 1_000_000, to: kind, ..Default::default() }),
        Address::ZERO,
    )
}
