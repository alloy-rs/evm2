//! Tracer errors must not change EVM execution or suppress faulting instructions.

use super::*;
use evm2_inspectors::tracing::js::JsInspectorError;
use serde_json::Value;

const TARGET: Address = Address::repeat_byte(0xaa);
const CHILD: Address = Address::repeat_byte(0xbb);

fn execute(
    script: &str,
    code: &[u8],
    gas_limit: u64,
) -> (Result<Value, JsInspectorError>, ResultAndState) {
    execute_with_child(script, code, &hex!("600160005500"), gas_limit)
}

fn execute_with_child(
    script: &str,
    code: &[u8],
    child: &[u8],
    gas_limit: u64,
) -> (Result<Value, JsInspectorError>, ResultAndState) {
    let mut db = CacheDB::new(EmptyDB::default());
    for (address, code, balance) in [
        (Address::ZERO, &[][..], U256::from(1e18)),
        (TARGET, code, U256::from(5)),
        (CHILD, child, U256::ZERO),
    ] {
        db.insert_account_info(
            &address,
            AccountInfo {
                balance,
                code: Some(Bytecode::new_raw(code.to_vec().into())),
                ..Default::default()
            },
        );
    }
    let inspector = JsInspector::new(script.into(), Value::Null).unwrap();
    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
        .with_db(db)
        .build_mainnet_with_inspector(inspector);
    let tx = TxEnv { kind: TransactTo::Call(TARGET), gas_limit, ..Default::default() };
    let result = evm.inspect_tx(tx.clone()).unwrap();
    let (ctx, inspector) = evm.ctx_inspector();
    let block = *ctx.block();
    let tx = tx.envelope();
    let output = inspector.json_result(&result.tx_result, &tx, &block, ctx.db_mut());
    (output, result)
}

fn error_message(error: JsInspectorError) -> String {
    let JsInspectorError::JsError(error) = error else { panic!("unexpected error: {error}") };
    let native = error.as_native().unwrap();
    format!("{}: {}", native.kind(), native.message())
}

fn error_events_tracer(has_step: bool) -> String {
    let step = if has_step {
        "step:function(l){if(l.getError())this.e.push(['step',l.op.toString(),l.getError()])},"
    } else {
        ""
    };
    format!(
        "{{e:[],{step}fault:function(l){{this.e.push(['fault',l.op.toString(),l.getError()])}},result:function(){{return this.e}}}}"
    )
}

#[test]
fn faults_follow_steps_and_preserve_instruction_metadata() {
    let script = "{e:[],step:function(l){this.e.push(['step',l.getPC(),l.op.toString(),l.getError()||null])},fault:function(l){this.e.push(['fault',l.getPC(),l.op.toString(),l.getError()])},result:function(c){return [this.e,c.error||null]}}";
    for (code, gas_limit, expected) in [
        (
            &hex!("60ff56")[..],
            1_000_000,
            json!([
                [
                    ["step", 0, "PUSH1", null],
                    ["step", 2, "JUMP", null],
                    ["fault", 2, "JUMP", "invalid jump destination"]
                ],
                "invalid jump destination"
            ]),
        ),
        (
            &hex!("01")[..],
            1_000_000,
            json!([[["step", 0, "ADD", "stack underflow"]], "stack underflow"]),
        ),
        (&hex!("6001")[..], 21_002, json!([[["step", 0, "PUSH1", "out of gas"]], "out of gas"])),
    ] {
        let (actual, result) = execute(script, code, gas_limit);
        assert!(!result.result.is_success());
        assert_eq!(actual.unwrap(), expected);
    }
    // Geth fills the log in its step hook, so a tracer without `step` sees an empty one.
    let fault_only = "{e:[],fault:function(l){this.e.push([l.getPC(),l.op.toString(),l.getCost(),l.getDepth(),l.stack.length(),l.memory.length(),l.getError()])},result:function(){return this.e}}";
    assert_eq!(
        execute(fault_only, &hex!("60ff5600"), 1_000_000).0.unwrap(),
        json!([[0, "STOP", 0, 0, 0, 0, "invalid jump destination"]])
    );
}

#[test]
fn oversized_initcode_is_reported_by_step() {
    for (code, opcode) in
        [(&hex!("61c00160006000f0")[..], "CREATE"), (&hex!("600061c00160006000f5")[..], "CREATE2")]
    {
        for has_step in [true, false] {
            let script = error_events_tracer(has_step);
            let (output, result) = execute(&script, code, 1_000_000);
            assert!(!result.result.is_success());
            assert_eq!(
                output.unwrap(),
                if has_step {
                    json!([["step", opcode, "max initcode size exceeded"]])
                } else {
                    json!([])
                },
            );
        }
    }
}

#[test]
fn static_write_errors_follow_the_opcode_validation_stage() {
    let mut parent = hex!("600060006000600073").to_vec();
    parent.extend_from_slice(CHILD.as_slice());
    parent.extend_from_slice(&hex!("61fffffa00"));
    for (child, opcode, callback) in [
        (&hex!("6000600055")[..], "SSTORE", "step"),
        (&hex!("600060006000f0")[..], "CREATE", "step"),
        (&hex!("6000600060006000f5")[..], "CREATE2", "step"),
        (&hex!("6000ff")[..], "SELFDESTRUCT", "step"),
        (&hex!("60006000600060006001600061fffff1")[..], "CALL", "step"),
        (&hex!("600060005d")[..], "TSTORE", "fault"),
        (&hex!("60006000a0")[..], "LOG0", "fault"),
    ] {
        for has_step in [true, false] {
            let script = error_events_tracer(has_step);
            let (output, result) = execute_with_child(&script, &parent, child, 1_000_000);
            assert!(result.result.is_success());
            assert_eq!(
                output.unwrap(),
                if has_step {
                    json!([[callback, opcode, "write protection"]])
                } else if callback == "fault" {
                    // A tracer without `step` sees geth's empty log.
                    json!([[callback, "STOP", "write protection"]])
                } else {
                    json!([])
                },
                "{opcode}, step callback: {has_step}",
            );
        }
    }
}

#[test]
fn hook_errors_preserve_execution() {
    let mut call = hex!("6000600060006000600073").to_vec();
    call.extend_from_slice(CHILD.as_slice());
    call.extend_from_slice(&hex!("61fffff150600160015500"));
    let baseline = "{fault:function(){},result:function(){return 42}}";
    for hook in ["step", "enter", "exit", "fault"] {
        let child = if hook == "fault" { &hex!("60ff56")[..] } else { &hex!("600160005500")[..] };
        let (_, expected) = execute_with_child(baseline, &call, child, 1_000_000);
        assert!(expected.result.is_success());
        let script = format!(
            "{{enter:function(){{}},exit:function(){{}},fault:function(){{}},result:function(){{return 42}},{hook}:function(){{let e=new AggregateError([], 'boom');e.errors.length=4294967295;throw e}}}}"
        );
        let (output, actual) = execute_with_child(&script, &call, child, 1_000_000);
        assert_eq!(
            error_message(output.unwrap_err()),
            format!("AggregateError: boom    in server-side tracer function '{hook}'")
        );
        assert_eq!(actual.tx_result.result, expected.tx_result.result);
        assert_eq!(actual.state, expected.state);
    }
}

#[test]
fn call_step_errors_preserve_execution() {
    let mut call = hex!("6000600060006000600073").to_vec();
    call.extend_from_slice(CHILD.as_slice());
    call.extend_from_slice(&hex!("61fffff100"));
    let baseline = "{fault:function(){},result:function(){return 42}}";
    let script = "{fault:function(){},result:function(){return 42},step:function(l){var o=l.op.toString();if(o=='CALL'||o=='CREATE')throw new Error('boom')}}";
    // The step ends before the child runs, which must still run with the gas it was given.
    for code in [&call[..], &hex!("5f5f5ff000")[..]] {
        let (_, expected) = execute(baseline, code, 1_000_000);
        let (output, actual) = execute(script, code, 1_000_000);
        assert_eq!(
            error_message(output.unwrap_err()),
            "Error: boom    in server-side tracer function 'step'"
        );
        assert_eq!(actual.tx_result.result, expected.tx_result.result);
        assert_eq!(actual.state, expected.state);
    }
}
