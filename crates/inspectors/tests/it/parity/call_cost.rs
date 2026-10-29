//! The cost of a call or creation step includes the gas it made available to the child frame.

use super::*;
use alloy_primitives::Bytes;
use alloy_rpc_types_trace::geth::GethDefaultTracingOptions;

#[test]
fn call_and_create_cost_includes_forwarded_gas() {
    // The child calls run `PUSH1 1 POP STOP` for 5 gas, the creations run `STOP`. `gas` is the
    // stack gas argument of a call, `None` for a creation. The value transfers fail their balance
    // precheck, since the caller has no balance, and enter no frame.
    let child = hex!("60015000");
    for (opcode, code, index, base, gas, stipend, child_used, entered) in [
        ("CALL", &hex!("5f5f5f5f5f604361fffff100")[..], 7, 2600, Some(0xffff), 0, 5, true),
        ("CALLCODE", &hex!("5f5f5f5f5f604361fffff200")[..], 7, 2600, Some(0xffff), 0, 5, true),
        ("DELEGATECALL", &hex!("5f5f5f5f604361fffff400")[..], 6, 2600, Some(0xffff), 0, 5, true),
        ("STATICCALL", &hex!("5f5f5f5f604361fffffa00")[..], 6, 2600, Some(0xffff), 0, 5, true),
        // The stack gas argument exceeds the 63/64 cap.
        ("CALL", &hex!("5f5f5f5f5f604363fffffffff100")[..], 7, 2600, Some(u64::MAX), 0, 5, true),
        // The value cost is charged and the stipend returned, but the stipend is not a cost.
        ("CALL", &hex!("5f5f5f5f6001604361fffff100")[..], 7, 11600, Some(0xffff), 2300, 0, false),
        ("CREATE", &hex!("60015f5ff000")[..], 3, 32005, None, 0, 0, true),
        ("CREATE2", &hex!("5f60015f5ff500")[..], 4, 32011, None, 0, 0, true),
        ("CREATE", &hex!("60015f6001f000")[..], 3, 32005, None, 0, 0, false),
    ] {
        let inspector = inspect_code(code, &child, SpecId::PRAGUE, TracingInspectorConfig::all());

        let trace = inspector.clone().into_parity_builder().vm_trace();
        let before = trace.ops[index - 1].ex.as_ref().unwrap().used;
        let op = &trace.ops[index];
        assert_eq!(op.op.as_deref(), Some(opcode));
        let available = before - base;
        let forwarded = gas.unwrap_or(u64::MAX).min(available - available / 64);
        let cost = base + forwarded;
        assert_eq!(op.cost, cost, "{opcode}");
        assert_eq!(
            op.ex.as_ref().unwrap().used,
            before - cost + forwarded + stipend - child_used,
            "{opcode}"
        );
        assert_eq!(op.sub.is_some(), entered, "{opcode}");

        let geth = inspector.geth_builder().geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        let log = geth.struct_logs.iter().find(|log| log.depth == 1 && log.pc == op.pc as u64);
        assert_eq!(log.unwrap().gas_cost, cost, "{opcode}");
    }
}
