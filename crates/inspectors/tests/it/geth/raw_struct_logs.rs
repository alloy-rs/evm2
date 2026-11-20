//! The default struct logger writing its struct logs as JSON, compared with the typed struct logs.

use super::*;
use evm2::{
    NoopInspector,
    evm::precompile::PrecompileOutput,
    interpreter::{GasTracker, Host, InstrStop, Message, MessageExt, MessageKind, MessageResult},
    precompiles::{Precompile, PrecompileId},
};
use evm2_inspectors::tracing::{DebugInspectorError, DebugTrace, RawDefaultFrame, StructLogMode};

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
    let trace = assert_modes_match(&db_with(code), SpecId::AMSTERDAM, &tx, true);
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
    let tx = TxEnv { gas_limit: 16_000_000, ..call_contract() };
    let run = |spec, limit, limits: Option<TraceLimits>| {
        let opts = GethDebugTracingOptions {
            config: GethDefaultTracingOptions { limit, ..Default::default() },
            ..Default::default()
        };
        let mut inspector =
            DebugInspector::new_with_struct_log_mode(opts, StructLogMode::Raw).unwrap();
        if let Some(limits) = limits {
            inspector.set_limits(limits);
        }
        let mut evm = Context::mainnet()
            .with_db(db.clone())
            .modify_cfg_chained(|cfg| cfg.spec = spec)
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
        let logs = logs.iter().map(|log| log.get().to_owned()).collect::<Vec<_>>();
        Some((logs, inspector.limit_exceeded()))
    };

    for spec in [SpecId::OSAKA, SpecId::AMSTERDAM] {
        let (full, exceeded) = run(spec, None, None).unwrap();
        assert!(!exceeded);
        let len = full.iter().map(String::len).sum::<usize>();
        let limits = |bytes| TraceLimits::default().set_max_recorded_bytes(Some(bytes));
        assert_eq!(run(spec, None, Some(limits(len))).unwrap(), (full.clone(), false));
        if spec == SpecId::AMSTERDAM {
            let create = full.iter().position(|log| log.contains(r#""op":"CREATE""#)).unwrap();
            let prefix = &full[..=create];
            let through_create = prefix.iter().map(String::len).sum::<usize>();
            let limit = Some(through_create as u64 - 1);
            for behavior in [TraceLimitBehavior::Skip, TraceLimitBehavior::Halt] {
                let budget = limits(through_create).set_behavior(behavior);
                assert_eq!(run(spec, limit, Some(budget)), Some((prefix.to_vec(), false)));
            }
            let halt = |bytes| Some(limits(bytes).set_behavior(TraceLimitBehavior::Halt));
            assert_eq!(run(spec, limit, halt(through_create - 1)), None);
            assert_eq!(run(spec, Some(through_create as u64), halt(through_create)), None);
        }
        for budget in [0, 48, 1000, len / 2, len - 1] {
            let (limited, exceeded) = run(spec, None, Some(limits(budget))).unwrap();
            assert!(exceeded);
            let mut remaining = budget;
            let count = full
                .iter()
                .take_while(|log| {
                    if let Some(left) = remaining.checked_sub(log.len()) {
                        remaining = left;
                        true
                    } else {
                        false
                    }
                })
                .count();
            assert_eq!(limited, full[..count], "{spec:?}, {budget}");
            assert_eq!(
                run(spec, None, Some(limits(budget).set_behavior(TraceLimitBehavior::Halt))),
                None
            );
        }
    }
}

/// Runs a raw or typed tracer composed with another inspector.
fn trace_with_hooks(
    code: &[u8],
    depth: u16,
    mode: StructLogMode,
    limit: Option<u64>,
    hooks: (impl Inspector<BaseEvmTypes>, impl Inspector<BaseEvmTypes>),
    precompiles: Precompiles,
) -> Value {
    let spec = SpecId::OSAKA;
    let mut inspector = DebugInspector::new_with_struct_log_mode(
        GethDebugTracingOptions {
            config: GethDefaultTracingOptions { limit, ..Default::default() },
            ..Default::default()
        },
        mode,
    )
    .unwrap();
    let mut db = db_with(code.to_vec());
    let block = Default::default();
    let tx = call_contract().envelope();
    let mut evm =
        Evm::<BaseEvmTypes>::new(spec, block, ethereum_tx_registry(spec), db.clone(), precompiles);
    evm.set_initial_call_depth(depth);
    evm.set_inspector(((hooks.0, &mut inspector), hooks.1));
    let result = evm.transact(&tx).unwrap().detach();
    drop(evm);
    let trace = inspector.get_trace(None, &tx, &block, &result, &mut db).unwrap();
    serde_json::to_value(trace).unwrap()
}

struct StopOnPush;

impl Inspector<BaseEvmTypes> for StopOnPush {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        if interp.opcode() == op::PUSH0 {
            interp.set_stop(InstrStop::Stop);
        }
    }
}

/// Edits a native subcall after the tracer has observed its entry.
struct EditSubcallDepth;

impl Inspector<BaseEvmTypes> for EditSubcallDepth {
    fn call(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message,
    ) -> Option<MessageResult> {
        if message.destination == Address::with_last_byte(RETURNER) {
            message.depth += 1;
        }
        None
    }
}

#[test]
fn raw_struct_logs_follow_only_visible_frames_and_keep_stopped_steps() {
    let native = 0x42;
    let code = [call(op::CALL, native, SOME_GAS), vec![op::PUSH0, op::INVALID]].concat();
    let run = |depth, mode, limit| {
        let mut precompiles = Precompiles::base(SpecId::OSAKA);
        precompiles.as_map_mut().insert(Precompile::new(
            Address::with_last_byte(native),
            PrecompileId::custom("SUBCALL"),
            |host: &mut Evm<'_, BaseEvmTypes>, parent: &Message, _gas: &mut GasTracker| {
                let target = Address::with_last_byte(RETURNER);
                let mut child = MessageExt {
                    kind: MessageKind::StaticCall,
                    depth: parent.depth + 1,
                    caller: parent.destination,
                    destination: target,
                    call_target: target,
                    code_address: target,
                    code: host.load_account(&target, true, false)?.code,
                    gas_limit: parent.gas_limit,
                    ..Default::default()
                };
                let result = host.execute_message(&mut child)?;
                assert!(result.is_success());
                Ok(PrecompileOutput::new(result.output))
            },
        ));
        trace_with_hooks(&code, depth, mode, limit, (StopOnPush, EditSubcallDepth), precompiles)
    };
    for depth in [0, 7] {
        let raw = run(depth, StructLogMode::Raw, None);
        assert_eq!(raw, run(depth, StructLogMode::Typed, None));
        let logs = raw["structLogs"].as_array().unwrap();
        if depth == 0 {
            assert!(logs.iter().all(|log| log["depth"] == 1));
            assert_eq!(logs.last().unwrap()["op"], "PUSH0");
            assert_eq!(logs.last().unwrap()["gasCost"], 0);
        } else {
            assert!(logs.is_empty());
        }
    }
    // Hidden child steps still count toward the typed inspector's capture limit.
    for limit in (450..1200).step_by(17) {
        assert_eq!(
            run(0, StructLogMode::Raw, Some(limit)),
            run(0, StructLogMode::Typed, Some(limit)),
            "{limit}"
        );
    }
}

#[test]
fn raw_frame_serializes_its_actual_field_count() {
    for mask in 0..8 {
        let frame = RawDefaultFrame {
            failed: false,
            gas: 21_000,
            execution_gas_used: (mask & 1 != 0).then_some(0),
            state_gas_used: (mask & 2 != 0).then_some(0),
            gas_refund: (mask & 4 != 0).then_some(0),
            return_value: Bytes::new(),
            struct_logs: serde_json::value::RawValue::from_string("[]".into()).unwrap(),
        };
        let bytes = rmp_serde::to_vec_named(&frame).unwrap();
        rmp_serde::from_slice::<serde::de::IgnoredAny>(&bytes).unwrap();
    }
}

#[test]
fn a_second_root_replaces_the_first_roots_steps_and_storage() {
    let first = [call(op::CALL, WRITER, ALL_GAS), hex!("600260015500").to_vec()].concat();
    let second = [call(op::CALL, RETURNER, ALL_GAS), hex!("60075400").to_vec()].concat();
    let run = |mode, limit, include_first: bool| {
        let config =
            GethDefaultTracingOptions { limit, disable_stack: Some(true), ..Default::default() };
        let mut tracer = DebugInspector::new_with_struct_log_mode(
            GethDebugTracingOptions { config, ..Default::default() },
            mode,
        )
        .unwrap();
        let spec = SpecId::OSAKA;
        let mut evm = Evm::<BaseEvmTypes>::new(
            spec,
            Default::default(),
            ethereum_tx_registry(spec),
            db_with(Vec::new()),
            Precompiles::base(spec),
        );
        evm.set_inspector(&mut tracer);
        for code in include_first.then_some(&first).into_iter().chain([&second]) {
            let mut message = MessageExt {
                destination: CONTRACT,
                caller: CALLER,
                call_target: CONTRACT,
                code_address: CONTRACT,
                code: Bytecode::new_raw(code.clone().into()),
                gas_limit: 2_000_000,
                ..Default::default()
            };
            assert!(evm.execute_message(&mut message).unwrap().is_success());
        }
        drop(evm);
        match tracer {
            DebugInspector::Default(inspector, _) => serde_json::to_value(
                inspector.geth_builder().geth_traces(0, Bytes::new(), config).struct_logs,
            )
            .unwrap(),
            DebugInspector::RawDefault(mut inspector) => serde_json::from_str::<Value>(
                inspector.take_frame(&evm2::TxResult::<BaseEvmTypes>::default()).struct_logs.get(),
            )
            .unwrap(),
            _ => unreachable!(),
        }
    };
    let expected = run(StructLogMode::Typed, None, false);
    for mode in [StructLogMode::Typed, StructLogMode::Raw] {
        assert_eq!(run(mode, None, true), expected);
    }
    for limit in [48, 1_050, 4_000] {
        assert_eq!(
            run(StructLogMode::Raw, Some(limit), true),
            run(StructLogMode::Typed, Some(limit), true)
        );
    }
}

struct OverrideCreatedAddress;

impl Inspector<BaseEvmTypes> for OverrideCreatedAddress {
    fn create_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Message,
        result: &mut MessageResult,
    ) {
        result.created_address = Some(CONTRACT);
    }
}

#[test]
fn storage_tracks_the_account_that_ran_the_initcode() {
    let code =
        [hex!("6002600155").to_vec(), create(op::CREATE), hex!("60015400").to_vec()].concat();
    for mode in [StructLogMode::Typed, StructLogMode::Raw] {
        let frame = trace_with_hooks(
            &code,
            0,
            mode,
            None,
            (OverrideCreatedAddress, NoopInspector::default()),
            Precompiles::base(SpecId::OSAKA),
        );
        let stores = logs_of(&frame, "SSTORE");
        assert_eq!(stores[1]["depth"], 2);
        let slot = |key, value| json!({format!("{:#066x}", key): format!("{:#066x}", value)});
        assert_eq!(stores[1]["storage"], slot(7, 1));
        assert_eq!(logs_of(&frame, "SLOAD")[0]["storage"], slot(1, 2));
    }
}

struct RedirectStorage(Address);

impl Inspector<BaseEvmTypes> for RedirectStorage {
    fn create(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message,
    ) -> Option<MessageResult> {
        message.destination = self.0;
        None
    }

    fn call(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message,
    ) -> Option<MessageResult> {
        if message.destination == Address::with_last_byte(WRITER) {
            message.destination = self.0;
        }
        None
    }
}

#[test]
fn redirected_create_and_call_share_the_destination_storage() {
    let code = [create(op::CREATE), call(op::CALL, WRITER, ALL_GAS)].concat();
    for mode in [StructLogMode::Raw, StructLogMode::Typed] {
        let frame = trace_with_hooks(
            &code,
            0,
            mode,
            None,
            (NoopInspector::default(), RedirectStorage(Address::with_last_byte(0xb2))),
            Precompiles::base(SpecId::OSAKA),
        );
        let stores = logs_of(&frame, "SSTORE");
        assert_eq!(stores.len(), 2);
        assert_eq!(
            stores[1]["storage"],
            json!({
                format!("{:#066x}", 7): format!("{:#066x}", 1),
                format!("{:#066x}", 1): format!("{:#066x}", 2),
            }),
            "{mode:?}"
        );
    }
}

#[test]
fn cloning_a_raw_debug_inspector_starts_a_new_recording() {
    let config = GethDefaultTracingOptions { disable_stack: Some(true), ..Default::default() };
    let opts = GethDebugTracingOptions { config, ..Default::default() };
    let limit = TraceLimits::default().set_max_recorded_bytes(Some(128));
    let raw = DebugInspector::new_with_struct_log_mode(opts, StructLogMode::Raw)
        .unwrap()
        .with_limits(limit);
    let ctx = Context::mainnet().with_db(db_with(busy_code()));
    let mut first = ctx.clone().build_mainnet_with_inspector(raw);
    let tx = call_contract();
    let first_res = first.inspect_tx(tx.clone()).unwrap();
    let mut second = ctx.build_mainnet_with_inspector(first.inspector().try_clone().unwrap());
    let second_res = second.inspect_tx(tx).unwrap();
    let frame = |inspector: &mut DebugInspector, result: &evm2::TxResult| {
        let DebugInspector::RawDefault(raw) = inspector else { unreachable!() };
        assert!(raw.limit_exceeded());
        raw.take_frame(result)
    };
    let expected = frame(first.inspector(), &first_res.tx_result.result);
    assert_ne!(expected.struct_logs.get(), "[]");
    assert_eq!(
        frame(second.inspector(), &second_res.tx_result.result).struct_logs.get(),
        expected.struct_logs.get()
    );
}

#[derive(Default)]
struct SentryAfterCall {
    child_returned: bool,
}

impl Inspector<BaseEvmTypes> for SentryAfterCall {
    fn call_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message,
        _result: &mut MessageResult,
    ) {
        self.child_returned = message.depth == 1;
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        if core::mem::take(&mut self.child_returned) {
            interp.set_stop(InstrStop::ReentrancySentryOOG);
        }
    }
}

#[test]
fn call_stopped_after_its_child_has_zero_sentry_cost() {
    let code = call(op::CALL, RETURNER, SOME_GAS);
    let run = |mode| {
        trace_with_hooks(
            &code,
            0,
            mode,
            None,
            (SentryAfterCall::default(), NoopInspector::default()),
            Precompiles::base(SpecId::OSAKA),
        )
    };
    let raw = run(StructLogMode::Raw);
    assert_eq!(raw, run(StructLogMode::Typed));
    let call = logs_of(&raw, "CALL")[0];
    assert_eq!(call["gasCost"], 0);
    assert_eq!(call["error"], "Some(ReentrancySentryOOG)");
}
