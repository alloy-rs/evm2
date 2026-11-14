//! The default struct logger writing its struct logs as JSON, compared with the typed struct logs.

use super::*;
use evm2_inspectors::tracing::{DebugInspectorError, DebugTrace, StructLogMode};

const CALLER: Address = address!("1000000000000000000000000000000000000001");
const CONTRACT: Address = address!("1000000000000000000000000000000000000002");

/// Stores 2 in slot 1 and loads it.
const WRITER: u8 = 0xa1;
/// Returns a word of memory.
const RETURNER: u8 = 0xa2;
/// Reverts with a word of memory.
const REVERTER: u8 = 0xa3;
/// Runs the `INVALID` opcode.
const INVALID: u8 = 0xa4;
/// Loops until it runs out of gas.
const LOOPER: u8 = 0xa5;
/// Runs an opcode that is not defined.
const UNDEFINED: u8 = 0xa6;
/// Pops an empty stack.
const UNDERFLOW: u8 = 0xa7;
/// Calls the writer and the returner.
const NESTED: u8 = 0xa8;

/// Init code that stores 1 in slot 7 and deploys no code.
const INIT_CODE: [u8; 10] = hex!("600160075560006000f3");

/// Code that pushes all the remaining gas, to forward it to a call.
const ALL_GAS: &[u8] = &[op::GAS];
/// Code that pushes a bounded amount of gas, for a call that halts and so consumes it all.
const SOME_GAS: &[u8] = &[op::PUSH2, 0xff, 0xff];
/// Code that pushes little gas, for a call that loops until it runs out.
const LITTLE_GAS: &[u8] = &[op::PUSH2, 0x01, 0x00];

/// Returns the code that calls `target` with the gas that `gas` pushes and no input, output or
/// value, and pops the result.
fn call(opcode: u8, target: u8, gas: &[u8]) -> Vec<u8> {
    let mut code = vec![op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0];
    if matches!(opcode, op::CALL | op::CALLCODE) {
        code.extend([op::PUSH1, 0]);
    }
    code.extend([op::PUSH1, target]);
    code.extend(gas);
    code.extend([opcode, op::POP]);
    code
}

/// Returns the code that creates a contract from [`INIT_CODE`] with `opcode`.
fn create(opcode: u8) -> Vec<u8> {
    let mut code = vec![op::PUSH10];
    code.extend(INIT_CODE);
    code.extend([op::PUSH1, 0, op::MSTORE]);
    if opcode == op::CREATE2 {
        code.extend([op::PUSH1, 0]);
    }
    code.extend([op::PUSH1, 10, op::PUSH1, 22, op::PUSH1, 0, opcode, op::POP]);
    code
}

/// Returns a database with a funded caller, the contracts the tests call and `code` at
/// [`CONTRACT`].
fn db_with(code: Vec<u8>) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &CALLER,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    let nested = [call(op::CALL, WRITER, ALL_GAS), call(op::CALL, RETURNER, ALL_GAS)].concat();
    for (address, code) in [
        (CONTRACT, code),
        (Address::with_last_byte(WRITER), hex!("600260015560015450").to_vec()),
        (Address::with_last_byte(RETURNER), hex!("602a60005260206000f3").to_vec()),
        (Address::with_last_byte(REVERTER), hex!("602a60005260206000fd").to_vec()),
        (Address::with_last_byte(INVALID), hex!("6001fe").to_vec()),
        (Address::with_last_byte(LOOPER), hex!("5b600056").to_vec()),
        (Address::with_last_byte(UNDEFINED), hex!("0c").to_vec()),
        (Address::with_last_byte(UNDERFLOW), hex!("01").to_vec()),
        (Address::with_last_byte(NESTED), nested),
    ] {
        db.insert_account_info(
            &address,
            AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() },
        );
    }
    db
}

/// Returns a transaction that calls [`CONTRACT`].
fn call_contract() -> TxEnv {
    TxEnv {
        caller: CALLER,
        gas_limit: 3_000_000,
        kind: TxKind::Call(CONTRACT),
        ..Default::default()
    }
}

/// Returns code that exercises every field of a struct log: memory, calls of every kind to
/// contracts that return, revert and halt, a call to a precompile, storage, a refund and creates.
fn busy_code() -> Vec<u8> {
    [
        vec![op::PUSH1, 0x2a, op::PUSH1, 0, op::MSTORE],
        // Call the identity precompile with a word of memory, which leaves return data.
        vec![op::PUSH1, 0x20, op::PUSH1, 0x20, op::PUSH1, 0x20, op::PUSH1, 0, op::PUSH1, 0],
        [&[op::PUSH1, 4], SOME_GAS, &[op::CALL, op::POP]].concat(),
        call(op::CALL, RETURNER, ALL_GAS),
        call(op::STATICCALL, RETURNER, ALL_GAS),
        call(op::CALL, REVERTER, ALL_GAS),
        call(op::CALL, INVALID, SOME_GAS),
        call(op::CALL, LOOPER, LITTLE_GAS),
        call(op::CALL, UNDEFINED, SOME_GAS),
        call(op::CALL, UNDERFLOW, SOME_GAS),
        call(op::DELEGATECALL, WRITER, ALL_GAS),
        // The write of a static call halts it.
        call(op::STATICCALL, WRITER, SOME_GAS),
        call(op::CALL, NESTED, ALL_GAS),
        call(op::CALLCODE, WRITER, ALL_GAS),
        // Load the slot the delegate call wrote, then clear it for a refund.
        vec![op::PUSH1, 1, op::SLOAD, op::POP, op::PUSH1, 0, op::PUSH1, 1, op::SSTORE],
        create(op::CREATE),
        create(op::CREATE2),
        vec![op::PUSH1, 0x20, op::PUSH1, 0, op::RETURN],
    ]
    .concat()
}

/// Traces `tx` with the default struct logger in `mode` and returns the JSON of its trace.
fn trace(
    db: &InMemoryDB,
    spec: SpecId,
    tx: &TxEnv,
    config: GethDefaultTracingOptions,
    mode: StructLogMode,
) -> String {
    let opts = GethDebugTracingOptions { config, ..Default::default() };
    let inspector = DebugInspector::new_with_struct_log_mode(opts, mode).unwrap();
    let mut evm = Context::mainnet()
        .with_db(db.clone())
        .modify_cfg_chained(|cfg| cfg.spec = spec)
        .build_mainnet_with_inspector(inspector);
    let result = evm.inspect_tx(tx.clone()).unwrap();
    let (ctx, inspector) = evm.ctx_inspector();
    let block = *ctx.block();
    let trace =
        inspector.get_trace(None, &tx.envelope(), &block, &result.tx_result, ctx.db_mut()).unwrap();
    assert_eq!(matches!(trace, DebugTrace::RawDefault(_)), mode == StructLogMode::Raw);
    serde_json::to_string(&trace).unwrap()
}

/// Asserts that the raw struct logs of `tx` serialize to the JSON of the typed ones under every
/// option, and returns the trace with every option enabled.
fn assert_modes_match(db: &InMemoryDB, spec: SpecId, tx: &TxEnv, limits: bool) -> Value {
    let all = GethDefaultTracingOptions {
        enable_memory: Some(true),
        enable_return_data: Some(true),
        ..Default::default()
    };
    for config in [
        GethDefaultTracingOptions::default(),
        GethDefaultTracingOptions {
            disable_stack: Some(true),
            disable_storage: Some(true),
            ..Default::default()
        },
        GethDefaultTracingOptions {
            disable_stack: Some(true),
            enable_memory: Some(true),
            ..Default::default()
        },
        GethDefaultTracingOptions {
            disable_storage: Some(true),
            enable_return_data: Some(true),
            ..Default::default()
        },
        all,
    ] {
        let typed = trace(db, spec, tx, config, StructLogMode::Typed);
        assert_eq!(trace(db, spec, tx, config, StructLogMode::Raw), typed, "{config:?}");
        if !limits {
            continue;
        }
        // The limit counts the bytes of JSON of the struct logs.
        let len = typed.len() as u64;
        for limit in [0, 1, 48, 49, len / 7, len / 3, len / 2, len] {
            let config = GethDefaultTracingOptions { limit: Some(limit), ..config };
            assert_eq!(
                trace(db, spec, tx, config, StructLogMode::Raw),
                trace(db, spec, tx, config, StructLogMode::Typed),
                "{config:?}"
            );
        }
    }
    serde_json::from_str(&trace(db, spec, tx, all, StructLogMode::Raw)).unwrap()
}

/// Returns the struct logs of `trace` that run `op`.
fn logs_of<'a>(trace: &'a Value, op: &str) -> Vec<&'a Value> {
    trace["structLogs"].as_array().unwrap().iter().filter(|log| log["op"] == op).collect()
}

#[test]
fn raw_struct_logs_match_typed_struct_logs() {
    let db = db_with(busy_code());
    for spec in [SpecId::LONDON, SpecId::CANCUN, SpecId::OSAKA] {
        let trace = assert_modes_match(&db, spec, &call_contract(), true);
        assert_eq!(trace["failed"], false, "{spec:?}");

        // Guard that the code exercises what the comparison is meant to cover.
        let logs = trace["structLogs"].as_array().unwrap();
        for field in ["stack", "memory", "returnData", "storage", "refund", "error"] {
            assert!(logs.iter().any(|log| log.get(field).is_some()), "{spec:?} {field}");
        }
        assert!(logs.iter().any(|log| log["depth"] == 3), "{spec:?}");
        assert_eq!(logs_of(&trace, "opcode 0xc not defined").len(), 1, "{spec:?}");
        for op in ["CALL", "STATICCALL", "DELEGATECALL", "CALLCODE", "CREATE", "CREATE2"] {
            assert!(!logs_of(&trace, op).is_empty(), "{spec:?} {op}");
        }
        // A delegate call writes the storage of its caller, a static call writes none.
        let sstores = logs_of(&trace, "SSTORE");
        assert!(sstores.iter().any(|log| log["depth"] == 2 && log.get("storage").is_none()));
        assert!(sstores.iter().any(|log| log["depth"] == 2 && log.get("storage").is_some()));
    }
}

#[test]
fn raw_struct_logs_match_typed_struct_logs_of_roots() {
    // A root that halts.
    let halting = [vec![op::PUSH1, 1, op::PUSH1, 2, op::SSTORE], vec![op::INVALID]].concat();
    let trace = assert_modes_match(&db_with(halting), SpecId::OSAKA, &call_contract(), true);
    assert_eq!(trace["failed"], true);
    assert_eq!(logs_of(&trace, "INVALID")[0]["error"], "Some(InvalidFEOpcode)");

    // A root without code runs no step.
    let trace = assert_modes_match(&db_with(Vec::new()), SpecId::OSAKA, &call_contract(), true);
    assert_eq!(trace["failed"], false);
    assert_eq!(trace["structLogs"], json!([]));

    // A root that creates a contract.
    let tx = TxEnv { kind: TxKind::Create, data: INIT_CODE.to_vec().into(), ..call_contract() };
    let trace = assert_modes_match(&db_with(Vec::new()), SpecId::OSAKA, &tx, true);
    assert_eq!(trace["failed"], false);
    assert_eq!(logs_of(&trace, "SSTORE").len(), 1);

    // A root that runs out of gas before it executes, on the state gas of the account it funds.
    let tx = TxEnv {
        kind: TxKind::Call(Address::with_last_byte(0xee)),
        value: U256::ONE,
        gas_limit: 200_000,
        ..call_contract()
    };
    let trace = assert_modes_match(&db_with(Vec::new()), SpecId::AMSTERDAM, &tx, true);
    assert_eq!(trace["failed"], true);
    assert_eq!(trace["structLogs"], json!([]));
}

#[test]
fn raw_struct_logs_match_typed_struct_logs_with_state_gas() {
    let code = [
        // Create state, in this contract and in nested calls, then refill it.
        vec![op::PUSH1, 1, op::PUSH1, 0, op::SSTORE],
        call(op::CALL, NESTED, ALL_GAS),
        call(op::CALL, WRITER, ALL_GAS),
        call(op::CALL, RETURNER, ALL_GAS),
        vec![op::PUSH1, 0, op::PUSH1, 0, op::SSTORE],
        create(op::CREATE),
        vec![op::STOP],
    ]
    .concat();
    let tx = TxEnv { gas_limit: 16_000_000, ..call_contract() };
    // A call's state gas cost is only known once it returns, so the struct logs of the two modes
    // can end at different steps under a limit.
    let trace = assert_modes_match(&db_with(code), SpecId::AMSTERDAM, &tx, false);
    assert_eq!(trace["failed"], false);
    for field in ["executionGasUsed", "stateGasUsed", "gasRefund"] {
        assert!(trace.get(field).is_some(), "{field}");
    }

    let logs = trace["structLogs"].as_array().unwrap();
    assert!(logs.iter().all(|log| log.get("stateGasReservoir").is_some()));
    let state_gas_cost = |log: &Value| log.get("stateGasCost").and_then(Value::as_i64);
    // Both the call to the nested contract and its call to the writer report the state the writer
    // created. The later calls create none.
    let calls = logs_of(&trace, "CALL");
    assert_eq!(calls.len(), 5);
    let costs: Vec<_> =
        calls.iter().map(|log| (log["depth"].as_u64(), state_gas_cost(log))).collect();
    assert!(costs[0].1.is_some_and(|cost| cost > 0), "{costs:?}");
    assert_eq!(costs[1], (Some(2), costs[0].1), "{costs:?}");
    assert_eq!(costs[2..], [(Some(2), None), (Some(1), None), (Some(1), None)], "{costs:?}");
    assert!(state_gas_cost(logs_of(&trace, "CREATE")[0]).is_some_and(|cost| cost > 0));
    let sstores: Vec<_> = logs_of(&trace, "SSTORE").into_iter().map(state_gas_cost).collect();
    assert!(sstores.iter().any(|cost| cost.is_some_and(|cost| cost < 0)), "{sstores:?}");
}

#[test]
fn raw_struct_logs_are_reset_by_fuse() {
    let db = db_with(busy_code());
    let opts = GethDebugTracingOptions::default();
    let inspector = DebugInspector::new_with_struct_log_mode(opts, StructLogMode::Raw).unwrap();
    let mut evm = Context::mainnet()
        .with_db(db.clone())
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::OSAKA)
        .build_mainnet_with_inspector(inspector);
    // A transaction that runs out of gas, then a complete one.
    for gas_limit in [60_000, 3_000_000] {
        let tx = TxEnv { gas_limit, ..call_contract() };
        let result = evm.inspect_tx(tx.clone()).unwrap();
        let (ctx, inspector) = evm.ctx_inspector();
        let block = *ctx.block();
        let trace = inspector
            .get_trace(None, &tx.envelope(), &block, &result.tx_result, ctx.db_mut())
            .unwrap();
        assert_eq!(
            serde_json::to_string(&trace).unwrap(),
            self::trace(&db, SpecId::OSAKA, &tx, Default::default(), StructLogMode::Typed),
            "{gas_limit}"
        );
        // The struct logs are taken by the first call.
        let DebugTrace::RawDefault(frame) = inspector
            .get_trace(None, &tx.envelope(), &block, &result.tx_result, ctx.db_mut())
            .unwrap()
        else {
            panic!("expected a raw frame")
        };
        assert_eq!(frame.struct_logs.get(), "[]");
        assert!(matches!(
            inspector.get_result(None, &tx.envelope(), &block, &result.tx_result, ctx.db_mut()),
            Err(DebugInspectorError::RawStructLogs)
        ));
        inspector.fuse().unwrap();
    }
}

#[test]
fn raw_struct_logs_stop_at_the_byte_budget() {
    let db = db_with(busy_code());
    let tx = call_contract();
    let run = |limits: Option<TraceLimits>| {
        let opts = GethDebugTracingOptions::default();
        let mut inspector =
            DebugInspector::new_with_struct_log_mode(opts, StructLogMode::Raw).unwrap();
        if let Some(limits) = limits {
            inspector.set_limits(limits);
        }
        let mut evm = Context::mainnet()
            .with_db(db.clone())
            .modify_cfg_chained(|cfg| cfg.spec = SpecId::OSAKA)
            .build_mainnet_with_inspector(inspector);
        let result = evm.inspect_tx(tx.clone()).ok()?;
        let (_, inspector) = evm.ctx_inspector();
        let DebugInspector::RawDefault(inspector) = inspector else {
            panic!("expected the raw struct logger")
        };
        let frame = inspector.take_frame(&result.tx_result.result);
        let logs =
            serde_json::from_str::<Vec<Box<serde_json::value::RawValue>>>(frame.struct_logs.get())
                .unwrap();
        let len = logs.iter().map(|log| log.get().len()).sum::<usize>();
        Some((logs.len(), len, inspector.limit_exceeded()))
    };

    let (count, len, exceeded) = run(None).unwrap();
    assert!(!exceeded);
    let limits = |bytes| TraceLimits::default().set_max_recorded_bytes(Some(bytes));
    assert_eq!(run(Some(limits(len))), Some((count, len, false)));
    for budget in [0, 48, 1000, len / 2, len - 1] {
        let (limited_count, limited_len, exceeded) = run(Some(limits(budget))).unwrap();
        assert!(limited_count < count && limited_len <= budget && exceeded, "{budget}");
        assert_eq!(run(Some(limits(budget).set_behavior(TraceLimitBehavior::Halt))), None);
    }
}
