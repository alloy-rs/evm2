//! Serialized form of the trace types.

use evm2_inspectors::tracing::types::{CallTrace, CallTraceStep};

const FULL_STEP: &str = r#"{"pc":7,"op":96,"stack":["0x1"],"push_stack":["0x2"],"memory":"0x00ff","returndata":"0xabcd","gas_remaining":100,"gas_refund_counter":5,"storage_change":{"key":"0x1","value":"0x2","had_value":"0x3","reason":"SSTORE"},"gas_cost":3,"state_gas_cost":-4,"state_gas_reservoir":9,"status":"Revert","immediate_bytes":"0x80","decoded":{"InternalCall":[{"func_name":"f","args":["a"],"return_data":null},3]}}"#;
const EMPTY_STEP: &str = r#"{"pc":0,"op":0,"stack":null,"push_stack":null,"memory":null,"returndata":"0x","gas_remaining":0,"gas_refund_counter":0,"storage_change":null,"gas_cost":0,"state_gas_cost":null,"state_gas_reservoir":null,"status":null,"immediate_bytes":null,"decoded":null}"#;
const NO_SELFDESTRUCT: &str = r#""selfdestruct_address":null,"selfdestruct_refund_target":null,"selfdestruct_transferred_value":null"#;
const SELFDESTRUCT: &str = r#""selfdestruct_address":"0x3333333333333333333333333333333333333333","selfdestruct_refund_target":"0x4444444444444444444444444444444444444444","selfdestruct_transferred_value":"0x9""#;
// A selfdestruct frame of a remote trace can lack the refund target and the value.
const PARTIAL_SELFDESTRUCT: &str = r#""selfdestruct_address":"0x3333333333333333333333333333333333333333","selfdestruct_refund_target":null,"selfdestruct_transferred_value":null"#;

fn trace_json(selfdestruct: &str) -> String {
    format!(
        r#"{{"depth":3,"success":true,"caller":"0x1111111111111111111111111111111111111111","address":"0x2222222222222222222222222222222222222222","maybe_precompile":false,{selfdestruct},"kind":"DELEGATECALL","value":"0x5","data":"0x01","output":"0x02","bytecode":"0x6000","gas_used":1,"gas_limit":2,"gas_refund_counter":3,"status":"SelfDestruct","steps":[{FULL_STEP},{EMPTY_STEP}],"step_deltas":[{{"step":0,"memory":null,"store":null,"gas_remaining_after":8}}],"decoded":{{"label":"l","return_data":null,"call_data":null}}}}"#
    )
}

/// The boxed step and selfdestruct data serialize as fields of the step and of the call trace.
#[test]
fn call_trace_serializes_as_flat_structs() {
    for step in [FULL_STEP, EMPTY_STEP] {
        let value = serde_json::from_str::<CallTraceStep>(step).unwrap();
        assert_eq!(value.extra.is_some(), step == FULL_STEP);
        assert_eq!(serde_json::to_string(&value).unwrap(), step);
    }

    for selfdestruct in [NO_SELFDESTRUCT, SELFDESTRUCT, PARTIAL_SELFDESTRUCT] {
        let json = trace_json(selfdestruct);
        let value = serde_json::from_str::<CallTrace>(&json).unwrap();
        assert_eq!(value.selfdestruct.is_some(), selfdestruct != NO_SELFDESTRUCT);
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
    }
}
