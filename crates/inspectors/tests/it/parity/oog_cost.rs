//! The cost of a step that runs out of gas is the gas it consumed, in both trace formats.

use super::*;
use alloy_primitives::Bytes;
use alloy_rpc_types_trace::geth::GethDefaultTracingOptions;
use evm2::interpreter::InstrStop;
use test_case::test_case;

fn inspect_child(child: &[u8], gas: u16) -> TracingInspector {
    let mut parent = hex!("5f5f5f5f5f604361").to_vec();
    parent.extend(gas.to_be_bytes());
    parent.extend([op::CALL, op::STOP]);
    inspect_code(&parent, child, SpecId::PRAGUE, TracingInspectorConfig::all())
}

// `gas` is the child's gas limit. The last opcode of each child runs out of gas with `remaining`
// left, which it consumes entirely, except for the reentrancy sentry that halts before charging
// anything.
#[test_case("BALANCE", &hex!("604431")[..], 645, 642, 642, InstrStop::OutOfGas; "BALANCE")]
#[test_case("EXTCODESIZE", &hex!("60443b")[..], 645, 642, 642, InstrStop::OutOfGas; "EXTCODESIZE")]
#[test_case("EXTCODEHASH", &hex!("60443f")[..], 645, 642, 642, InstrStop::OutOfGas; "EXTCODEHASH")]
#[test_case(
    "EXTCODECOPY", &hex!("60015f5f60443c")[..], 652, 642, 642, InstrStop::OutOfGas;
    "EXTCODECOPY"
)]
#[test_case("SLOAD", &hex!("5f54")[..], 644, 642, 642, InstrStop::OutOfGas; "SLOAD")]
#[test_case("CALL", &hex!("5f5f5f5f5f60445af1")[..], 657, 642, 642, InstrStop::OutOfGas; "CALL")]
#[test_case(
    "CALLCODE", &hex!("5f5f5f5f5f60445af2")[..], 657, 642, 642, InstrStop::OutOfGas;
    "CALLCODE"
)]
#[test_case(
    "DELEGATECALL", &hex!("5f5f5f5f60445af4")[..], 655, 642, 642, InstrStop::OutOfGas;
    "DELEGATECALL"
)]
#[test_case(
    "STATICCALL", &hex!("5f5f5f5f60445afa")[..], 655, 642, 642, InstrStop::OutOfGas;
    "STATICCALL"
)]
#[test_case("PUSH1", &hex!("6001")[..], 2, 2, 2, InstrStop::OutOfGas; "PUSH1")]
#[test_case("SSTORE", &hex!("602a5f55")[..], 5005, 5000, 5000, InstrStop::OutOfGas; "SSTORE")]
#[test_case(
    "CALL", &hex!("5f5f5f5f600160445ff1")[..], 5016, 5000, 5000, InstrStop::OutOfGas;
    "CALL with value"
)]
#[test_case("LOG0", &hex!("60205fa0")[..], 390, 385, 385, InstrStop::OutOfGas; "LOG0")]
#[test_case(
    "SSTORE", &hex!("5f5f55")[..], 100, 96, 0, InstrStop::ReentrancySentryOOG;
    "SSTORE reentrancy sentry"
)]
fn out_of_gas_step_costs_the_consumed_gas(
    opcode: &str,
    code: &[u8],
    gas: u16,
    remaining: u64,
    cost: u64,
    status: InstrStop,
) {
    let inspector = inspect_child(code, gas);
    let step = inspector.traces().nodes()[1].trace.steps.last().unwrap();
    assert_eq!(step.status, Some(status));
    assert_eq!(step.gas_remaining, remaining);
    assert_eq!(step.gas_cost, cost);

    let geth =
        inspector.geth_builder().geth_traces(0, Bytes::new(), GethDefaultTracingOptions::default());
    let failed = geth.struct_logs.iter().rfind(|log| log.depth == 2).unwrap();
    assert_eq!(failed.opcode(), opcode);
    assert_eq!(failed.gas, remaining);
    assert_eq!(failed.gas_cost, cost);

    let trace = inspector.into_parity_builder().vm_trace();
    let failed = trace.ops[7].sub.as_ref().unwrap().ops.last().unwrap();
    assert_eq!(failed.op.as_deref(), Some(opcode));
    assert_eq!(failed.cost, cost);
    assert!(failed.ex.is_none() && failed.sub.is_none());
}
