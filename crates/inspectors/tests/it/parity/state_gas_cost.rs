//! Amsterdam step costs leave out state gas, in both trace formats, and a step's state gas leaves
//! out the state gas of the frame it starts.
//!
//! The expected values come from go-ethereum `a0cba163b1f17f6044df84502ff13036dffb5de1`
//! (`evm 1.17.11-unstable-a0cba163`): the interpreter passes each opcode's execution and state
//! gas separately (core/vm/interpreter.go) and the struct logger records the execution gas only
//! (core/tracing/hooks.go, `EmitOpcode`).

use super::*;

/// Transaction gas limit that leaves a reservoir of 100,000 gas, below the 183,600 gas of an
/// account creation: the gas above the 2^24 transaction cap.
const SHORT_RESERVOIR_GAS_LIMIT: u64 = 16_777_216 + 100_000;
/// Transaction gas limit that leaves a reservoir larger than any state charge below.
const FULL_RESERVOIR_GAS_LIMIT: u64 = 20_000_000;

/// Returns the cost of each step of `code` in parity `vmTrace` and in geth struct logs, which
/// must agree, and the state gas of each step.
fn step_costs(code: &[u8], balance: u64, gas_limit: u64) -> (Vec<u64>, Vec<Option<i64>>) {
    let inspect =
        |config| inspect_funded_code(code, &[], SpecId::AMSTERDAM, config, balance, gas_limit);
    let inspector = inspect(TracingInspectorConfig::all());
    let steps = &inspector.traces().nodes()[0].trace.steps;
    let costs = steps.iter().map(|step| step.gas_cost).collect::<Vec<_>>();
    let state_costs = steps.iter().map(|step| step.state_gas_cost).collect();
    for config in [
        TracingInspectorConfig::parity_vm_trace(),
        TracingInspectorConfig::parity_vm_trace().set_step_deltas(false),
    ] {
        let vm_trace = inspect(config).into_parity_builder().vm_trace();
        assert_eq!(vm_trace.ops.iter().map(|op| op.cost).collect::<Vec<_>>(), costs);
        // The gas left after a step is the gas the next step starts with.
        for (op, next) in vm_trace.ops.iter().zip(steps.iter().skip(1)) {
            assert_eq!(op.ex.as_ref().unwrap().used, next.gas_remaining);
        }
    }
    let geth = inspect(TracingInspectorConfig::default_geth()).geth_builder().geth_traces(
        0,
        Default::default(),
        Default::default(),
    );
    assert_eq!(geth.struct_logs.iter().map(|log| log.gas_cost).collect::<Vec<_>>(), costs);
    (costs, state_costs)
}

#[test]
fn sstore_cost_leaves_out_state_gas() {
    // A new slot costs 12,100 gas and 64 state bytes, drawn from the remaining gas when the
    // reservoir is empty.
    for gas_limit in [1_000_000, FULL_RESERVOIR_GAS_LIMIT] {
        let (costs, state_costs) = step_costs(&hex!("602a5f5500"), 0, gas_limit);
        assert_eq!(costs[2], 12_100);
        assert_eq!(state_costs[2], Some(64 * 1_530));
    }
}

#[test]
fn restoring_sstore_cost_leaves_out_state_gas_refill() {
    // Writing the original zero back refills the state gas of the first write, which returns
    // to the remaining gas when it spilled into it. The cost stays 100.
    for gas_limit in [1_000_000, SHORT_RESERVOIR_GAS_LIMIT, FULL_RESERVOIR_GAS_LIMIT] {
        let (costs, _) = step_costs(&hex!("602a5f555f5f5500"), 0, gas_limit);
        assert_eq!(costs[2], 12_100);
        assert_eq!(costs[5], 100);
    }
}

#[test]
fn selfdestruct_cost_leaves_out_state_gas() {
    // Sending the balance to a new account costs 17,000 gas plus the account creation state
    // gas, which the remaining gas pays when the reservoir is short.
    for (gas_limit, spilled) in [
        (1_000_000, NEW_ACCOUNT_STATE_GAS),
        (SHORT_RESERVOIR_GAS_LIMIT, NEW_ACCOUNT_STATE_GAS - 100_000),
        (FULL_RESERVOIR_GAS_LIMIT, 0),
    ] {
        let code = hex!("6044ff");
        let (costs, state_costs) = step_costs(&code, 16, gas_limit);
        assert_eq!(costs[1], 17_000);
        assert_eq!(state_costs[1], Some(NEW_ACCOUNT_STATE_GAS as i64));
        for config in [
            TracingInspectorConfig::parity_vm_trace(),
            TracingInspectorConfig::parity_vm_trace().set_step_deltas(false),
        ] {
            let inspector =
                inspect_funded_code(&code, &[], SpecId::AMSTERDAM, config, 16, gas_limit);
            let remaining = inspector.traces().nodes()[0].trace.steps[1].gas_remaining;
            let trace = inspector.into_parity_builder().vm_trace();
            assert_eq!(trace.ops[1].ex.as_ref().unwrap().used, remaining - 17_000 - spilled);
        }
    }
}

#[test]
fn value_call_to_new_account_cost_leaves_out_state_gas() {
    // A cold CALL with value forwarding 0xffff gas to a new account. The child returns its gas
    // and the stipend.
    let code = hex!("5f5f5f5f6001604461fffff100");
    for (gas_limit, spilled) in [
        (1_000_000, NEW_ACCOUNT_STATE_GAS),
        (SHORT_RESERVOIR_GAS_LIMIT, NEW_ACCOUNT_STATE_GAS - 100_000),
        (FULL_RESERVOIR_GAS_LIMIT, 0),
    ] {
        let step = call_step_funded(&code, &[], SpecId::AMSTERDAM, 16, gas_limit);
        assert_eq!(step.cost, 3_000 + 11_300 + 65_535);
        assert_eq!(step.used, step.gas_remaining - step.cost - spilled + 65_535 + 2_300);
    }
}

#[test]
fn create_cost_leaves_out_state_gas_spilled_from_a_short_reservoir() {
    for (gas_limit, spilled) in [
        (1_000_000, NEW_ACCOUNT_STATE_GAS),
        (SHORT_RESERVOIR_GAS_LIMIT, NEW_ACCOUNT_STATE_GAS - 100_000),
        (FULL_RESERVOIR_GAS_LIMIT, 0),
    ] {
        let step = call_step_funded(&hex!("5f5f5ff000"), &[], SpecId::AMSTERDAM, 0, gas_limit);
        assert_eq!(step.cost, 12_000);
        assert_eq!(step.used, step.gas_remaining - 12_000 - spilled);
    }
}

/// Returns the state gas of the call or create step before the final `STOP` in the root frame,
/// and the state gas of the `SSTORE` that the child frame runs as its third step.
fn call_and_child_sstore_state_gas(code: &[u8], child: &[u8]) -> (Option<i64>, Option<i64>) {
    let inspector = inspect_funded_code(
        code,
        child,
        SpecId::AMSTERDAM,
        TracingInspectorConfig::all(),
        16,
        FULL_RESERVOIR_GAS_LIMIT,
    );
    let nodes = inspector.traces().nodes();
    let steps = &nodes[0].trace.steps;
    (steps[steps.len() - 2].state_gas_cost, nodes[1].trace.steps[2].state_gas_cost)
}

#[test]
fn call_state_gas_leaves_out_the_state_gas_of_the_child() {
    // The value call reaches an account with code, so it is not a new account and the call step
    // charges no state gas. The child writes a new slot: 64 state bytes at 1,530 gas per byte.
    let code = hex!("5f5f5f5f6001604361fffff100");
    let (call, child_sstore) = call_and_child_sstore_state_gas(&code, &hex!("602a5f5500"));
    assert_eq!(child_sstore, Some(64 * 1_530));
    assert_eq!(call, None);
}

#[test]
fn create_state_gas_leaves_out_the_state_gas_of_the_initcode() {
    // The create step charges the account creation. The initcode, `PUSH1 0x2a PUSH0 SSTORE
    // STOP`, writes a new slot and runs as the child frame.
    let code = hex!("64602a5f55005f526005601b5ff000");
    let (create, child_sstore) = call_and_child_sstore_state_gas(&code, &[]);
    assert_eq!(child_sstore, Some(64 * 1_530));
    assert_eq!(create, Some(NEW_ACCOUNT_STATE_GAS as i64));
}
