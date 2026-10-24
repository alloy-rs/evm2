//! LOG requirements are reported even when charging stops before the dynamic part.

use super::*;
use alloy_primitives::Bytes;
use alloy_rpc_types_trace::geth::GethDefaultTracingOptions;
use evm2::interpreter::InstrStop;

fn inspect_log(code: &[u8], gas: u16, config: TracingInspectorConfig) -> TracingInspector {
    let mut parent = hex!("5f5f5f5f5f604361").to_vec();
    parent.extend(gas.to_be_bytes());
    parent.extend([op::CALL, op::STOP]);
    inspect_code(&parent, code, SpecId::PRAGUE, config)
}

#[test]
fn failed_logs_report_the_full_requirement() {
    // Geth 1df30b18d9ed35ee9d28ffd4648442c2204554ad, `evm run --trace`, Prague.
    // A failed LOG reports base + topics + data + memory, not the remaining gas or
    // just the first unsuccessful deduction. The enclosing frame still burns its limit.
    for topics in 0..=4 {
        for (remaining, offset, len, expansion) in [
            (123, 0, 0, 0),
            (123, 64, 32, 9),
            (400, 64, 32, 9),
            (375 + 375 * u16::from(topics) + 256 + 8, 64, 32, 9),
        ] {
            let mut code = Vec::new();
            for _ in 0..topics {
                code.extend([op::PUSH1, 0]);
            }
            code.extend([op::PUSH1, len, op::PUSH1, offset, op::LOG0 + topics]);
            let gas = remaining + 3 * (u16::from(topics) + 2);
            let expected = 375 + 375 * u64::from(topics) + 8 * u64::from(len) + expansion;
            for config in [TracingInspectorConfig::parity_vm_trace(), TracingInspectorConfig::all()]
            {
                let inspector = inspect_log(&code, gas, config);
                let child = &inspector.traces().nodes()[1].trace;
                let step = child.steps.last().unwrap();
                assert!(step.status.unwrap().is_out_of_gas(), "{topics} {remaining}");
                assert_eq!(step.gas_remaining, u64::from(remaining));
                assert_eq!(step.gas_cost, expected, "{topics} {remaining}");
                assert_eq!(child.gas_used, u64::from(gas));
                assert!(inspector.traces().nodes()[1].logs.is_empty());

                let geth = inspector.geth_builder().geth_traces(
                    0,
                    Bytes::new(),
                    GethDefaultTracingOptions::default(),
                );
                let failed = geth
                    .struct_logs
                    .iter()
                    .find(|step| step.depth == 2 && step.error.is_some())
                    .unwrap();
                assert_eq!(failed.gas_cost, expected);

                let trace = inspector.into_parity_builder().vm_trace();
                let failed = trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap();
                assert_eq!(failed.cost, expected);
                assert!(failed.ex.is_none());
            }
        }
    }
}

#[test]
fn log_cost_capture_preserves_other_outcomes() {
    for (code, gas, expected, status) in [
        (&hex!("600060006000a100")[..], 1000, 750, None),
        (&hex!("60006000a1")[..], 1000, 750, Some(InstrStop::StackUnderflow)),
    ] {
        let inspector = inspect_log(code, gas, TracingInspectorConfig::all());
        let child = &inspector.traces().nodes()[1].trace;
        let log = child.steps.iter().find(|step| step.op.get() == op::LOG1).unwrap();
        assert_eq!(log.gas_cost, expected);
        assert_eq!(log.status, status);
    }
}

#[test]
fn failed_log_only_includes_new_memory() {
    // Allocate 145 words before LOG1(offset=0x10e8, len=0x120), leaving 123 gas.
    let inspector = inspect_log(
        &hex!("60006112005260006101206110e8a1"),
        617,
        TracingInspectorConfig::parity_vm_trace(),
    );
    let trace = inspector.into_parity_builder().vm_trace();
    let child = trace.ops[7].sub.as_ref().unwrap();
    assert_eq!(child.ops[child.ops.len() - 2].ex.as_ref().unwrap().used, 123);
    let log = child.ops.last().unwrap();
    assert_eq!(log.cost, 3054);
    assert!(log.ex.is_none());
}

#[test]
fn zero_length_log_ignores_an_unrepresentable_offset() {
    let mut code = hex!("60007f").to_vec();
    code.extend([0xff; 32]);
    code.push(op::LOG0);
    let inspector = inspect_log(&code, 129, TracingInspectorConfig::parity_vm_trace());
    let trace = inspector.into_parity_builder().vm_trace();
    let log = trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap();
    assert_eq!(log.cost, 375);
    assert!(log.ex.is_none());
}

#[test]
fn static_log_retains_its_execution_error() {
    let inspector = inspect_code(
        &hex!("5f5f5f5f604361fffffa00"),
        &hex!("600060006000a1"),
        SpecId::PRAGUE,
        TracingInspectorConfig::all(),
    );
    let log = inspector.traces().nodes()[1].trace.steps.last().unwrap();
    assert_eq!(log.status, Some(InstrStop::StateChangeDuringStaticCall));
    assert_eq!(log.gas_cost, 375);
}
