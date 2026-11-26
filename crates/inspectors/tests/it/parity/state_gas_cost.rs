//! Amsterdam step costs leave out state gas, in both trace formats, the JavaScript tracer's
//! `getCost` and the opcode gas inspector.
//!
//! The expected values come from go-ethereum `a0cba163b1f17f6044df84502ff13036dffb5de1`
//! (`evm 1.17.11-unstable-a0cba163`): the interpreter passes each opcode's execution and state
//! gas separately (core/vm/interpreter.go) and the struct logger records the execution gas only
//! (core/tracing/hooks.go, `EmitOpcode`).

use super::*;
use evm2::interpreter::opcode::OpCode;
use evm2_inspectors::opcode::OpcodeGasInspector;

#[cfg(feature = "js-tracer")]
use evm2_inspectors::tracing::js::JsInspector;

/// Transaction gas limit that leaves a reservoir of 100,000 gas, below the 183,600 gas of an
/// account creation: the gas above the 2^24 transaction cap.
const SHORT_RESERVOIR_GAS_LIMIT: u64 = 16_777_216 + 100_000;
/// Transaction gas limit that leaves a reservoir larger than any state charge below.
const FULL_RESERVOIR_GAS_LIMIT: u64 = 20_000_000;
/// Transaction gas limit that leaves a reservoir of 50,000 gas, below the state gas of a new
/// storage slot.
const HALF_SLOT_RESERVOIR_GAS_LIMIT: u64 = 16_777_216 + 50_000;
/// State gas of a new storage slot: 64 state bytes at 1530 gas per byte.
const NEW_SLOT_STATE_GAS: u64 = 64 * 1_530;

/// Returns the cost of each step of `code` in parity `vmTrace` and in geth struct logs, which
/// must agree, and the state gas of each step.
fn step_costs(code: &[u8], balance: u64, gas_limit: u64) -> (Vec<u64>, Vec<Option<i64>>) {
    let inspect =
        |config| inspect_funded_code(code, &[], SpecId::AMSTERDAM, config, balance, gas_limit);
    let inspector = inspect(TracingInspectorConfig::all());
    let steps = &inspector.traces().nodes()[0].trace.steps;
    let costs = steps.iter().map(|step| step.gas_cost).collect::<Vec<_>>();
    let state_costs = steps.iter().map(|step| step.state_gas_cost()).collect();
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

#[test]
fn step_tracers_agree_on_a_spilled_sstore() {
    // The struct logger and the VM trace (which `step_costs` holds equal), the opcode gas
    // inspector and the JavaScript tracer's `getCost` cost each step alike, leaving out the new
    // slot's state gas whether it spills past the reservoir in full, in part or not at all. The
    // spill shows in the gas the next step starts with.
    let code = hex!("602a5f5500");
    for (gas_limit, spilled) in [
        (1_000_000, NEW_SLOT_STATE_GAS),
        (HALF_SLOT_RESERVOIR_GAS_LIMIT, NEW_SLOT_STATE_GAS - 50_000),
        (FULL_RESERVOIR_GAS_LIMIT, 0),
    ] {
        let (costs, state_costs) = step_costs(&code, 0, gas_limit);
        assert_eq!(costs, [3, 2, 12_100, 0], "gas limit {gas_limit}");
        assert_eq!(state_costs[2], Some(NEW_SLOT_STATE_GAS as i64));
        let config = TracingInspectorConfig::default_geth();
        let inspector = inspect_funded_code(&code, &[], SpecId::AMSTERDAM, config, 0, gas_limit);
        let steps = &inspector.traces().nodes()[0].trace.steps;
        assert_eq!(steps[3].gas_remaining, steps[2].gas_remaining - 12_100 - spilled);

        let mut counter = OpcodeGasInspector::default();
        drop(run_funded_code(&code, &[], SpecId::AMSTERDAM, &mut counter, 0, gas_limit));
        let opcode_gas = counter.opcode_gas();
        for (opcode, cost) in [(op::PUSH1, 3), (op::PUSH0, 2), (op::SSTORE, 12_100)] {
            assert_eq!(opcode_gas[&OpCode::new(opcode).unwrap()], cost, "gas limit {gas_limit}");
        }

        #[cfg(feature = "js-tracer")]
        {
            let tracer = r#"{
                costs: [],
                step: function(log) { this.costs.push(log.getCost()); },
                fault: function() {},
                result: function() { return this.costs; }
            }"#;
            let js = JsInspector::new(tracer.to_string(), serde_json::Value::Null).unwrap();
            let (mut evm, res) = run_funded_code(&code, &[], SpecId::AMSTERDAM, js, 0, gas_limit);
            let (context, js) = evm.ctx_inspector();
            let (tx, block) = (context.tx().envelope(), *context.block());
            let js_costs = js.json_result(&res.tx_result, &tx, &block, context.db_mut()).unwrap();
            assert_eq!(js_costs, serde_json::json!(costs), "gas limit {gas_limit}");
        }
    }
}

#[test]
fn opcode_gas_of_a_call_leaves_out_its_child_state_gas_spill() {
    // A cold CALL forwarding all it can to a child that writes a new slot, then stops, reverts
    // or halts. The CALL's own gas is its cold access alone, whether the child's state gas
    // spills past the reservoir or not, and the child's SSTORE costs its 12,100.
    let code = hex!("5f5f5f5f5f60435af100");
    for child in [hex!("602a5f5500").as_slice(), &hex!("602a5f555f5ffd"), &hex!("602a5f55fe")] {
        for gas_limit in [1_000_000, FULL_RESERVOIR_GAS_LIMIT] {
            let mut counter = OpcodeGasInspector::default();
            drop(run_funded_code(&code, child, SpecId::AMSTERDAM, &mut counter, 0, gas_limit));
            let opcode_gas = counter.opcode_gas();
            let gas = |opcode| opcode_gas[&OpCode::new(opcode).unwrap()];
            let case = format!("child {child:02x?}, gas limit {gas_limit}");
            assert_eq!(gas(op::CALL), 3_000, "{case}");
            assert_eq!(gas(op::SSTORE), 12_100, "{case}");
        }
    }
}

#[test]
fn opcode_gas_of_a_create_leaves_out_its_child_state_gas_spill() {
    // A CREATE whose initcode writes a new slot, then stops, reverts or halts. The CREATE's own
    // gas is what the struct logger gives it, whether the child's state gas spills past the
    // reservoir or not.
    for initcode in [hex!("602a5f5500").as_slice(), &hex!("602a5f555f5ffd"), &hex!("602a5f55fe")] {
        // PUSHn initcode, PUSH0, MSTORE, then CREATE(0, 32 - len, len).
        let len = initcode.len() as u8;
        let mut code = vec![0x5f + len];
        code.extend_from_slice(initcode);
        code.extend_from_slice(&[op::PUSH0, op::MSTORE, op::PUSH1, len, op::PUSH1, 32 - len]);
        code.extend_from_slice(&[op::PUSH0, op::CREATE, op::STOP]);
        for gas_limit in [1_000_000, FULL_RESERVOIR_GAS_LIMIT] {
            let config = TracingInspectorConfig::default_geth();
            let inspector =
                inspect_funded_code(&code, &[], SpecId::AMSTERDAM, config, 0, gas_limit);
            let create = inspector.traces().nodes()[0]
                .trace
                .steps
                .iter()
                .find(|step| step.op.get() == op::CREATE)
                .unwrap();
            let mut counter = OpcodeGasInspector::default();
            drop(run_funded_code(&code, &[], SpecId::AMSTERDAM, &mut counter, 0, gas_limit));
            let opcode_gas = counter.opcode_gas();
            let case = format!("initcode {initcode:02x?}, gas limit {gas_limit}");
            assert_eq!(opcode_gas[&OpCode::new(op::CREATE).unwrap()], create.gas_cost, "{case}");
            assert_eq!(opcode_gas[&OpCode::new(op::SSTORE).unwrap()], 12_100, "{case}");
        }
    }
}
