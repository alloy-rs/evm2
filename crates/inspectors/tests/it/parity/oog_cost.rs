//! Costs for out-of-gas guards and failed gas deductions.

use super::*;
use alloy_primitives::Bytes;
use alloy_rpc_types_trace::geth::GethDefaultTracingOptions;
use evm2::interpreter::InstrStop;

fn inspect_child(child: &[u8], gas: u16) -> TracingInspector {
    let mut parent = hex!("5f5f5f5f5f604361").to_vec();
    parent.extend(gas.to_be_bytes());
    parent.extend([op::CALL, op::STOP]);
    inspect_code(&parent, child, SpecId::PRAGUE, TracingInspectorConfig::all())
}

#[test]
fn cold_load_guards_consume_the_remaining_step_gas() {
    // Each child reaches its final opcode with 642 gas. The cold-load guard fails after
    // the warm base (and, for EXTCODECOPY, copy/memory gas), without a failed deduction.
    for (code, gas, opcode, geth_cost) in [
        (&hex!("604431")[..], 645, "BALANCE", 2600),
        (&hex!("60443b")[..], 645, "EXTCODESIZE", 2600),
        (&hex!("60443f")[..], 645, "EXTCODEHASH", 2600),
        (&hex!("60015f5f60443c")[..], 652, "EXTCODECOPY", 2606),
        (&hex!("5f54")[..], 644, "SLOAD", 2100),
        (&hex!("5f5f5f5f5f60445af1")[..], 657, "CALL", 100),
        (&hex!("5f5f5f5f5f60445af2")[..], 657, "CALLCODE", 100),
        (&hex!("5f5f5f5f60445af4")[..], 655, "DELEGATECALL", 100),
        (&hex!("5f5f5f5f60445afa")[..], 655, "STATICCALL", 100),
    ] {
        let inspector = inspect_child(code, gas);
        let step = inspector.traces().nodes()[1].trace.steps.last().unwrap();
        assert_eq!(step.status, Some(InstrStop::OutOfGas), "{opcode}");
        assert_eq!(step.gas_remaining, 642, "{opcode}");
        assert_eq!(step.gas_cost, 642, "{opcode}");

        let geth = inspector.geth_builder().geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        let failed =
            geth.struct_logs.iter().rfind(|log| log.depth == 2 && log.opcode() == opcode).unwrap();
        assert_eq!(failed.gas, 642, "{opcode}");
        assert_eq!(failed.gas_cost, geth_cost, "{opcode}");

        let trace = inspector.into_parity_builder().vm_trace();
        let failed = trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap();
        assert_eq!(failed.op.as_deref(), Some(opcode));
        assert_eq!(failed.cost, 642, "{opcode}");
        assert!(failed.ex.is_none() && failed.sub.is_none(), "{opcode}");
    }
}

#[test]
fn attempted_charges_and_uncharged_sentry_are_preserved() {
    for (code, gas, remaining, cost, status) in [
        (&hex!("6001")[..], 0, 0, 3, InstrStop::OutOfGas),
        (&hex!("602a5f55")[..], 5005, 5000, 22100, InstrStop::OutOfGas),
        (&hex!("5f5f55")[..], 100, 96, 0, InstrStop::ReentrancySentryOOG),
    ] {
        let inspector = inspect_child(code, gas);
        let step = inspector.traces().nodes()[1].trace.steps.last().unwrap();
        assert_eq!(step.status, Some(status));
        assert_eq!(step.gas_remaining, remaining);
        assert_eq!(step.gas_cost, cost);
        let trace = inspector.into_parity_builder().vm_trace();
        let failed = trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap();
        assert_eq!(failed.cost, cost);
        assert!(failed.ex.is_none());
    }
}

#[test]
fn cold_access_cost_overrides_do_not_leak_into_other_steps() {
    for (code, gas, opcode, cost, status) in [
        // The static charge fails before any host access.
        (&hex!("604431")[..], 53, "BALANCE", 100, Some(InstrStop::OutOfGas)),
        (&hex!("60443b")[..], 53, "EXTCODESIZE", 100, Some(InstrStop::OutOfGas)),
        (&hex!("60443f")[..], 53, "EXTCODEHASH", 100, Some(InstrStop::OutOfGas)),
        (&hex!("60015f5f60443c")[..], 60, "EXTCODECOPY", 100, Some(InstrStop::OutOfGas)),
        (&hex!("5f5f5f5f5f60445ff1")[..], 65, "CALL", 100, Some(InstrStop::OutOfGas)),
        (&hex!("5f5f5f5f5f60445ff2")[..], 65, "CALLCODE", 100, Some(InstrStop::OutOfGas)),
        (&hex!("5f5f5f5f60445ff4")[..], 63, "DELEGATECALL", 100, Some(InstrStop::OutOfGas)),
        (&hex!("5f5f5f5f60445ffa")[..], 63, "STATICCALL", 100, Some(InstrStop::OutOfGas)),
        // Successful cold and warm reads retain their charged cost.
        (&hex!("60443100")[..], 10_000, "BALANCE", 2600, None),
        (&hex!("60443b00")[..], 10_000, "EXTCODESIZE", 2600, None),
        (&hex!("60443f00")[..], 10_000, "EXTCODEHASH", 2600, None),
        (&hex!("60015f5f60443c00")[..], 10_000, "EXTCODECOPY", 2606, None),
        (&hex!("5f5400")[..], 10_000, "SLOAD", 2100, None),
        (&hex!("5f5f5f5f5f60445ff100")[..], 10_000, "CALL", 2600, None),
        (&hex!("5f5f5f5f5f60445ff200")[..], 10_000, "CALLCODE", 2600, None),
        (&hex!("5f5f5f5f60445ff400")[..], 10_000, "DELEGATECALL", 2600, None),
        (&hex!("5f5f5f5f60445ffa00")[..], 10_000, "STATICCALL", 2600, None),
        (&hex!("60433100")[..], 645, "BALANCE", 100, None),
        (&hex!("60433b00")[..], 645, "EXTCODESIZE", 100, None),
        (&hex!("60433f00")[..], 645, "EXTCODEHASH", 100, None),
        (&hex!("60015f5f60433c00")[..], 652, "EXTCODECOPY", 106, None),
        (&hex!("5f54505f5400")[..], 2748, "SLOAD", 100, None),
        // Ordinary failed deduction and failed memory expansion remain distinct.
        (&hex!("602a5f55")[..], 5005, "SSTORE", 22100, Some(InstrStop::OutOfGas)),
        (&hex!("5f61040052")[..], 100, "MSTORE", 104, Some(InstrStop::MemoryOOG)),
    ] {
        let inspector = inspect_child(code, gas);
        let step = inspector.traces().nodes()[1]
            .trace
            .steps
            .iter()
            .rfind(|step| step.op.as_str() == opcode)
            .unwrap();
        assert_eq!(step.status, status, "{opcode}");
        assert_eq!(step.gas_cost, cost, "{opcode}");
        assert_eq!(step.geth_gas_cost, None, "{opcode}");
        let geth = inspector.geth_builder().geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        let log =
            geth.struct_logs.iter().rfind(|log| log.depth == 2 && log.opcode() == opcode).unwrap();
        assert_eq!(log.gas_cost, cost, "{opcode}");
        let trace = inspector.into_parity_builder().vm_trace();
        let entry = trace.ops[7]
            .sub
            .as_ref()
            .unwrap()
            .ops
            .iter()
            .rfind(|entry| entry.op.as_deref() == Some(opcode))
            .unwrap();
        assert_eq!(entry.cost, cost, "{opcode}");
    }
}

#[test]
fn skipped_cold_load_cost_does_not_require_snapshots() {
    let inspector = inspect_code(
        &hex!("5f5f5f5f5f604361028cf100"),
        &hex!("60015f5f60443c"),
        SpecId::OSAKA,
        TracingInspectorConfig::from_geth_config(&GethDefaultTracingOptions {
            disable_stack: Some(true),
            ..Default::default()
        }),
    );
    let step = inspector.traces().nodes()[1].trace.steps.last().unwrap();
    assert!(step.stack.is_none() && step.memory.is_none());
    assert_eq!(step.gas_remaining, 642);
    assert_eq!(step.gas_cost, 642);
    assert_eq!(step.geth_gas_cost, Some(2606));
}

#[test]
fn skipped_cold_load_metadata_is_cleared_when_reusing_a_frame() {
    let inspector = inspect_code(
        &hex!("5f5f5f5f5f6043610285f1505f5f5f5f5f6043612710f100"),
        &hex!("60443100"),
        SpecId::OSAKA,
        TracingInspectorConfig::all(),
    );
    let nodes = inspector.traces().nodes();
    assert_eq!(nodes[1].trace.steps[1].geth_gas_cost, Some(2600));
    assert_eq!(nodes[2].trace.steps[1].geth_gas_cost, None);
    assert_eq!(nodes[2].trace.steps[1].gas_cost, 2600);
    assert!(nodes[0].trace.steps.iter().all(|step| step.geth_gas_cost.is_none()));
}

#[test]
fn sload_cost_before_the_static_charge() {
    // Both the cold first read and a warm second read reach SLOAD with 50 gas.
    for (code, gas, cost) in [(&hex!("5f54")[..], 52, 2100), (&hex!("5f54505f54")[..], 2156, 100)] {
        let inspector = inspect_child(code, gas);
        let step = inspector.traces().nodes()[1].trace.steps.last().unwrap();
        assert_eq!(step.status, Some(InstrStop::OutOfGas));
        assert_eq!(step.gas_remaining, 50);
        assert_eq!(step.gas_cost, 100);
        let geth = inspector.geth_builder().geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        assert_eq!(
            geth.struct_logs
                .iter()
                .rfind(|log| log.depth == 2 && log.opcode() == "SLOAD")
                .unwrap()
                .gas_cost,
            cost
        );
        let trace = inspector.into_parity_builder().vm_trace();
        assert_eq!(trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap().cost, 100);
    }
}
