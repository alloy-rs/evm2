//! Gas accounting shared by Geth struct logs and Parity vm traces.

use super::*;
use evm2::interpreter::opcode::OpCode;
use evm2_inspectors::tracing::OpcodeFilter;

#[test]
fn geth_create_cost_uses_its_own_child_after_filtered_call() {
    let config = TracingInspectorConfig {
        record_opcodes_filter: Some(OpcodeFilter::new().enabled(OpCode::CREATE)),
        ..TracingInspectorConfig::default_geth()
    };
    let inspector = inspect_code(
        &hex!("5f5f5f5f5f604361fffff1505f5f5ff000"),
        &hex!("00"),
        SpecId::OSAKA,
        config,
    );
    let geth = inspector.geth_builder().geth_traces(0, Default::default(), Default::default());
    assert_eq!(geth.struct_logs.len(), 1);
    assert_eq!(geth.struct_logs[0].opcode(), "CREATE");
    assert_eq!(geth.struct_logs[0].gas_cost, 32_000);
}

#[test]
fn vmtrace_post_call_gas_does_not_require_memory_or_storage_deltas() {
    for code in [
        &hex!("5f5f5f5f5f604361fffff100")[..],
        &hex!("5f5f5f5f5f604361fffff200")[..],
        &hex!("5f5f5f5f604361fffff400")[..],
        &hex!("5f5f5f5f604361fffffa00")[..],
        &hex!("5f5f5ff000")[..],
        &hex!("5f5f5f5ff500")[..],
    ] {
        let full = inspect_code(
            code,
            &hex!("5f5ffd"),
            SpecId::OSAKA,
            TracingInspectorConfig::parity_vm_trace(),
        )
        .into_parity_builder()
        .vm_trace();
        let index = full.ops.len() - 2;
        let expected = &full.ops[index];
        for config in [
            TracingInspectorConfig::parity_vm_trace().set_step_deltas(false),
            TracingInspectorConfig::default_geth(),
        ] {
            for limit in [None, core::num::NonZeroU64::new(index as u64 + 1)] {
                let trace = inspect_code(
                    code,
                    &hex!("5f5ffd"),
                    SpecId::OSAKA,
                    TracingInspectorConfig { step_limit: limit, ..config },
                )
                .into_parity_builder()
                .vm_trace();
                assert_eq!(trace.ops[index].cost, expected.cost);
                assert_eq!(
                    trace.ops[index].ex.as_ref().unwrap().used,
                    expected.ex.as_ref().unwrap().used,
                    "code={code:02x?}, limit={limit:?}"
                );
            }
        }
    }
}
