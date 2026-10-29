//! The cost of a step that runs out of gas is the gas it consumed, in both trace formats.

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
fn out_of_gas_step_costs_the_consumed_gas() {
    // `gas` is the child's gas limit. The last opcode of each child runs out of gas with
    // `remaining` left, which it consumes entirely, except for the reentrancy sentry that halts
    // before charging anything.
    for (opcode, code, gas, remaining, cost, status) in [
        ("BALANCE", &hex!("604431")[..], 645, 642, 642, InstrStop::OutOfGas),
        ("EXTCODESIZE", &hex!("60443b")[..], 645, 642, 642, InstrStop::OutOfGas),
        ("EXTCODEHASH", &hex!("60443f")[..], 645, 642, 642, InstrStop::OutOfGas),
        ("EXTCODECOPY", &hex!("60015f5f60443c")[..], 652, 642, 642, InstrStop::OutOfGas),
        ("SLOAD", &hex!("5f54")[..], 644, 642, 642, InstrStop::OutOfGas),
        ("CALL", &hex!("5f5f5f5f5f60445af1")[..], 657, 642, 642, InstrStop::OutOfGas),
        ("CALLCODE", &hex!("5f5f5f5f5f60445af2")[..], 657, 642, 642, InstrStop::OutOfGas),
        ("DELEGATECALL", &hex!("5f5f5f5f60445af4")[..], 655, 642, 642, InstrStop::OutOfGas),
        ("STATICCALL", &hex!("5f5f5f5f60445afa")[..], 655, 642, 642, InstrStop::OutOfGas),
        ("PUSH1", &hex!("6001")[..], 2, 2, 2, InstrStop::OutOfGas),
        ("SSTORE", &hex!("602a5f55")[..], 5005, 5000, 5000, InstrStop::OutOfGas),
        ("CALL", &hex!("5f5f5f5f600160445ff1")[..], 5016, 5000, 5000, InstrStop::OutOfGas),
        ("LOG0", &hex!("60205fa0")[..], 390, 385, 385, InstrStop::OutOfGas),
        ("SSTORE", &hex!("5f5f55")[..], 100, 96, 0, InstrStop::ReentrancySentryOOG),
    ] {
        let inspector = inspect_child(code, gas);
        let step = inspector.traces().nodes()[1].trace.steps.last().unwrap();
        assert_eq!(step.status, Some(status), "{opcode}");
        assert_eq!(step.gas_remaining, remaining, "{opcode}");
        assert_eq!(step.gas_cost, cost, "{opcode}");

        let geth = inspector.geth_builder().geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        let failed = geth.struct_logs.iter().rfind(|log| log.depth == 2).unwrap();
        assert_eq!(failed.opcode(), opcode);
        assert_eq!(failed.gas, remaining, "{opcode}");
        assert_eq!(failed.gas_cost, cost, "{opcode}");

        let trace = inspector.into_parity_builder().vm_trace();
        let failed = trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap();
        assert_eq!(failed.op.as_deref(), Some(opcode));
        assert_eq!(failed.cost, cost, "{opcode}");
        assert!(failed.ex.is_none() && failed.sub.is_none(), "{opcode}");
    }
}
