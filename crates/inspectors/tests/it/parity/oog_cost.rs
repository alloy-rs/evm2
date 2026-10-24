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
    for (code, gas, opcode) in [
        (&hex!("604431")[..], 645, "BALANCE"),
        (&hex!("60443b")[..], 645, "EXTCODESIZE"),
        (&hex!("60443f")[..], 645, "EXTCODEHASH"),
        (&hex!("60015f5f60443c")[..], 652, "EXTCODECOPY"),
        (&hex!("5f54")[..], 644, "SLOAD"),
        (&hex!("5f5f5f5f5f60445af1")[..], 657, "CALL"),
        (&hex!("5f5f5f5f5f60445af2")[..], 657, "CALLCODE"),
        (&hex!("5f5f5f5f60445af4")[..], 655, "DELEGATECALL"),
        (&hex!("5f5f5f5f60445afa")[..], 655, "STATICCALL"),
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
            geth.struct_logs.iter().find(|log| log.depth == 2 && log.opcode() == opcode).unwrap();
        assert_eq!(failed.gas, 642, "{opcode}");
        assert_eq!(failed.gas_cost, 642, "{opcode}");

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
