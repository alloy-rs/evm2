use super::*;
use crate::tracing::types::{
    CallTrace, CallTraceStep, RecordedMemory, StorageChange, StorageChangeReason,
};
use alloc::string::String;
use evm2::interpreter::{InstrStop, opcode::OpCode};

fn test_step(pc: usize, op: u8) -> CallTraceStep {
    CallTraceStep {
        pc,
        op: OpCode::new_or_unknown(op),
        stack: Some(vec![U256::from(pc)].into_boxed_slice()),
        push_stack: None,
        memory: None,
        returndata: Bytes::from(vec![pc as u8]),
        gas_remaining: 1000 - pc as u64,
        gas_refund_counter: 0,
        gas_used: pc as u64,
        gas_cost: 1,
        state_gas_cost: None,
        state_gas_reservoir: None,
        state_gas_spent: 0,
        storage_change: (op == op::SSTORE).then(|| {
            alloc::boxed::Box::new(StorageChange {
                key: U256::ZERO,
                value: U256::from(pc),
                had_value: None,
                reason: StorageChangeReason::SSTORE,
            })
        }),
        storage_slot: (op == op::SSTORE).then(|| Box::new((U256::ZERO, U256::from(pc)))),
        status: (op == op::REVERT).then_some(InstrStop::Revert),
        immediate_bytes: None,
        contract_size: None,
        decoded: None,
    }
}

#[test]
fn struct_log_errors_exclude_reverts() {
    for (status, has_error) in [
        (None, false),
        (Some(InstrStop::Stop), false),
        (Some(InstrStop::Return), false),
        (Some(InstrStop::Revert), false),
        (Some(InstrStop::OutOfFunds), false),
        (Some(InstrStop::CallTooDeep), false),
        (Some(InstrStop::OutOfGas), true),
        (Some(InstrStop::StackUnderflow), true),
        (Some(InstrStop::OpcodeNotFound), true),
    ] {
        let mut step = test_step(0, op::REVERT);
        step.status = status;
        let log = step.convert_to_geth_struct_log(&Default::default(), 1, None);
        assert_eq!(log.error.is_some(), has_error, "{status:?}");
    }
}

#[test]
fn struct_log_omits_zero_refund_and_empty_memory() {
    let opts = GethDefaultTracingOptions::default().enable_memory();
    let mut step = test_step(0, op::PUSH0);
    step.memory = Some(RecordedMemory::new(&[]));
    let log = step.convert_to_geth_struct_log(&opts, 1, None);
    assert_eq!((log.refund_counter, log.memory), (None, None));

    step.gas_refund_counter = 4800;
    step.memory = Some(RecordedMemory::new(&[1]));
    let log = step.convert_to_geth_struct_log(&opts, 1, None);
    assert_eq!(log.refund_counter, Some(4800));
    assert_eq!(log.memory.map(|memory| memory.len()), Some(1));
}

#[test]
fn erc7562_keccak_preimages_require_successful_steps() {
    for status in [
        None,
        Some(InstrStop::OutOfGas),
        Some(InstrStop::MemoryOOG),
        Some(InstrStop::MemoryLimitOOG),
        Some(InstrStop::PrecompileOOG),
        Some(InstrStop::InvalidOperandOOG),
        Some(InstrStop::ReentrancySentryOOG),
        Some(InstrStop::StackUnderflow),
        Some(InstrStop::FatalExternalError),
    ] {
        for (offset, len, expected) in
            [(0, 0, vec![]), (1, 2, vec![2, 3]), (1, 4, vec![2, 3, 0, 0]), (4, 2, vec![0, 0])]
        {
            let mut step = test_step(0, op::KECCAK256);
            step.stack = Some(vec![U256::from(len), U256::from(offset)].into_boxed_slice());
            step.memory = Some(RecordedMemory::new(&[1, 2, 3]));
            step.status = status;
            let builder = GethTraceBuilder::new(vec![CallTraceNode {
                trace: CallTrace {
                    steps: vec![step, test_step(1, op::REVERT)],
                    status: Some(InstrStop::Revert),
                    ..Default::default()
                },
                ..Default::default()
            }]);
            let frame = builder.geth_erc7562_traces(Erc7562Config::default(), 0);
            let expected = if status.is_none() { vec![Bytes::from(expected)] } else { vec![] };
            assert_eq!(frame.keccak, expected, "status {status:?}, offset {offset}, len {len}");
        }
    }
}

#[test]
fn erc7562_classifies_all_out_of_gas_statuses() {
    for (status, expected) in [
        (None, false),
        (Some(InstrStop::Stop), false),
        (Some(InstrStop::Return), false),
        (Some(InstrStop::Revert), false),
        (Some(InstrStop::OutOfFunds), false),
        (Some(InstrStop::OpcodeNotFound), false),
        (Some(InstrStop::OutOfGas), true),
        (Some(InstrStop::MemoryOOG), true),
        (Some(InstrStop::MemoryLimitOOG), true),
        (Some(InstrStop::PrecompileOOG), true),
        (Some(InstrStop::InvalidOperandOOG), true),
        (Some(InstrStop::ReentrancySentryOOG), true),
    ] {
        assert_eq!(status.is_some_and(InstrStop::is_out_of_gas), expected);
        for frame_status in [false, true] {
            let mut step = test_step(0, op::KECCAK256);
            step.status = status;
            let builder = GethTraceBuilder::new(vec![CallTraceNode {
                trace: CallTrace {
                    status: if frame_status { status } else { None },
                    steps: if frame_status { vec![] } else { vec![step] },
                    ..Default::default()
                },
                ..Default::default()
            }]);
            for ignored in [false, true] {
                let mut config = Erc7562Config::default();
                if ignored {
                    config.ignored_opcodes.push(op::KECCAK256);
                }
                let frame = builder.geth_erc7562_traces(config, 0);
                assert_eq!(
                    frame.out_of_gas, expected,
                    "status {status:?}, frame_status {frame_status}, ignored {ignored}"
                );
            }
        }
    }
}

#[test]
fn opcode_trace_resumes_parents_after_nested_and_empty_calls() {
    // Cover all call-like opcodes, a precompile with no steps, an empty creation,
    // a reverting grandchild, and a final call opcode that failed before recording a child.
    type Fixture = (Option<usize>, usize, &'static [usize], &'static [u8]);
    let fixtures: &[Fixture] = &[
        (
            None,
            0,
            &[1, 2, 4, 5, 6],
            &[
                op::SSTORE,
                op::CALL,
                op::CALLCODE,
                op::STATICCALL,
                op::CREATE,
                op::CREATE2,
                op::CALL,
            ],
        ),
        (Some(0), 1, &[], &[]),
        (Some(0), 1, &[3], &[op::SSTORE, op::DELEGATECALL, op::SLOAD, op::STOP]),
        (Some(2), 2, &[], &[op::SSTORE, op::REVERT]),
        (Some(0), 1, &[], &[op::RETURN]),
        (Some(0), 1, &[], &[]),
        (Some(0), 1, &[], &[op::STOP]),
    ];
    let mut nodes: Vec<_> = fixtures
        .iter()
        .enumerate()
        .map(|(idx, &(parent, depth, children, ops))| {
            CallTraceNode {
                idx,
                parent,
                children: children.to_vec(),
                trace: CallTrace {
                    depth,
                    maybe_precompile: Some(idx == 1),
                    address: Address::with_last_byte(idx as u8),
                    // The grandchild executes in its parent's storage context.
                    caller: Address::with_last_byte(2),
                    kind: if idx == 3 { CallKind::DelegateCall } else { CallKind::Call },
                    steps: ops
                        .iter()
                        .enumerate()
                        .map(|(pc, &op)| test_step(idx * 10 + pc, op))
                        .collect(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .collect();
    nodes[0].trace.steps.last_mut().unwrap().status = Some(InstrStop::OutOfGas);
    // The SLOAD reads the value its delegatecall child wrote.
    nodes[2].trace.steps[2].storage_slot = Some(Box::new((U256::ZERO, U256::from(30))));
    let builder = GethTraceBuilder::new(nodes);
    let expected = [
        (0, 1),
        (1, 1),
        (2, 1),
        (20, 2),
        (21, 2),
        (30, 3),
        (31, 3),
        (22, 2),
        (23, 2),
        (3, 1),
        (40, 2),
        (4, 1),
        (5, 1),
        (60, 2),
        (6, 1),
    ];
    for disable_storage in [false, true] {
        let opts = GethDefaultTracingOptions {
            disable_storage: Some(disable_storage),
            enable_return_data: Some(true),
            ..Default::default()
        };
        let frame = builder.geth_traces(123, Bytes::new(), opts);
        assert_eq!(frame.gas, 123);
        assert_eq!(
            frame.struct_logs.iter().map(|log| (log.pc, log.depth)).collect::<Vec<_>>(),
            expected
        );
        for log in &frame.struct_logs {
            assert_eq!(log.stack, Some(vec![U256::from(log.pc)]));
            assert_eq!(log.return_data, Some(Bytes::from(vec![log.pc as u8])));
            assert_eq!(log.gas, 1000 - log.pc);
            let value = match log.pc {
                0 => Some(0),
                20 => Some(20),
                30 | 22 => Some(30),
                _ => None,
            };
            assert_eq!(
                log.storage,
                value
                    .filter(|_| !disable_storage)
                    .map(|value| { BTreeMap::from([(B256::ZERO, U256::from(value).into())]) })
            );
        }
        let sizes: Vec<_> = frame.struct_logs.iter().map(struct_log_json_len).collect();
        let total: usize = sizes.iter().sum();
        for limit in
            [0, 1, sizes[0], sizes[0] + sizes[1], sizes[..5].iter().sum(), total, 2 * total]
        {
            let limited = builder.geth_traces(
                123,
                Bytes::new(),
                GethDefaultTracingOptions {
                    limit: Some(limit as u64),
                    disable_storage: Some(disable_storage),
                    enable_return_data: Some(true),
                    ..Default::default()
                },
            );
            let mut written = 0;
            let len = if limit == 0 {
                expected.len()
            } else {
                sizes
                    .iter()
                    .take_while(|size| {
                        let logged = written <= limit;
                        written += **size;
                        logged
                    })
                    .count()
            };
            assert_eq!(limited.struct_logs, frame.struct_logs[..len], "limit {limit}");
        }
    }
    assert!(
        GethTraceBuilder::new(vec![])
            .geth_traces(0, Bytes::new(), Default::default())
            .struct_logs
            .is_empty()
    );
    assert!(
        GethTraceBuilder::new(vec![CallTraceNode::default()])
            .geth_traces(0, Bytes::new(), Default::default())
            .struct_logs
            .is_empty()
    );
}

#[test]
fn storage_snapshots_accumulate_and_skip_failed_steps() {
    let mut failed = test_step(4, op::SSTORE);
    failed.status = Some(InstrStop::OutOfGas);
    let mut sload = test_step(1, op::SLOAD);
    sload.storage_slot = Some(Box::new((U256::ZERO, U256::from(1))));
    // A step that is not an SLOAD or SSTORE has no slot.
    let mut add = test_step(2, op::ADD);
    add.storage_slot = None;
    let builder = GethTraceBuilder::new(vec![CallTraceNode {
        trace: CallTrace {
            steps: vec![test_step(0, op::PUSH0), sload, add, failed],
            ..Default::default()
        },
        ..Default::default()
    }]);
    for disable_storage in [false, true] {
        let frame = builder.geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions {
                disable_storage: Some(disable_storage),
                ..Default::default()
            },
        );
        assert_eq!(frame.struct_logs.len(), 4);
        assert!(frame.struct_logs[0].storage.is_none());
        assert_eq!(
            frame.struct_logs[1].storage,
            (!disable_storage).then(|| BTreeMap::from([(B256::ZERO, U256::from(1).into())]))
        );
        assert!(frame.struct_logs[2].storage.is_none());
        assert!(frame.struct_logs[3].storage.is_none());
    }
}

/// The entries are what go-ethereum master's struct logger writes (`debug_traceCall`).
#[test]
fn struct_log_json_len_matches_geth() {
    for json in [
        r#"{"pc":0,"op":"ADD","gas":979000,"gasCost":3,"depth":1,"error":"stack underflow (0 \u003c=\u003e 2)","stack":[]}"#,
        r#"{"pc":4,"op":"SSTORE","gas":978994,"gasCost":12100,"depth":1,"stack":["0x1","0x0"],"storage":{"0x0000000000000000000000000000000000000000000000000000000000000000":"0x0000000000000000000000000000000000000000000000000000000000000001"}}"#,
        r#"{"pc":7,"op":"SLOAD","gas":868971,"gasCost":100,"depth":1,"stack":["0x0"],"storage":{"0x0000000000000000000000000000000000000000000000000000000000000000":"0x0000000000000000000000000000000000000000000000000000000000000001"}}"#,
        r#"{"pc":11,"op":"PUSH1","gas":868862,"gasCost":3,"depth":1,"stack":[],"memory":["0x0000000000000000000000000000000000000000000000000000000000000001"]}"#,
        r#"{"pc":13,"op":"MLOAD","gas":868859,"gasCost":3,"depth":1,"stack":["0x0"],"memory":["0x0000000000000000000000000000000000000000000000000000000000000001"]}"#,
        r#"{"pc":14,"op":"PUSH1","gas":868856,"gasCost":3,"depth":1,"stack":["0x1"],"memory":["0x0000000000000000000000000000000000000000000000000000000000000001"]}"#,
        r#"{"pc":0,"op":"PUSH1","gas":979000,"gasCost":3,"depth":1,"stack":[]}"#,
        r#"{"pc":4,"op":"SSTORE","gas":978994,"gasCost":12100,"depth":1,"stack":["0x0","0x5"],"storage":{"0x0000000000000000000000000000000000000000000000000000000000000005":"0x0000000000000000000000000000000000000000000000000000000000000000"},"refund":11616}"#,
        r#"{"pc":33,"op":"STOP","gas":978997,"gasCost":0,"depth":1,"stack":["0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff60"]}"#,
        r#"{"pc":1,"op":"STOP","gas":5,"gasCost":0,"depth":1,"returnData":"0xdeadbeef","refund":4800}"#,
        r#"{"pc":20,"op":"opcode 0xc not defined","gas":5,"gasCost":0,"depth":12,"error":"gas uint64 overflow \u0026 more"}"#,
    ] {
        let log: StructLog = serde_json::from_str(json).unwrap();
        assert_eq!(struct_log_json_len(&log), json.len(), "{json}");
    }
}

#[test]
fn struct_log_json_len_skips_the_fields_geth_omits() {
    let log = StructLog {
        pc: 1,
        op: "STOP".into(),
        gas: 5,
        depth: 1,
        error: Some(String::new()),
        return_data: Some(Bytes::new()),
        memory: Some(vec![]),
        storage: Some(BTreeMap::new()),
        refund_counter: Some(0),
        ..Default::default()
    };
    let json = r#"{"pc":1,"op":"STOP","gas":5,"gasCost":0,"depth":1}"#;
    assert_eq!(struct_log_json_len(&log), json.len());
}

#[test]
fn struct_log_json_len_counts_the_state_gas_fields() {
    for (cost, reservoir) in [(None, None), (Some(0), Some(0)), (Some(-1234), Some(99_999))] {
        let log = StructLog {
            op: "SSTORE".into(),
            gas: 100,
            depth: 2,
            state_gas_cost: cost,
            state_gas_reservoir: reservoir,
            ..Default::default()
        };
        assert_eq!(
            struct_log_json_len(&log),
            serde_json::to_string(&log).unwrap().len(),
            "{cost:?} {reservoir:?}"
        );
    }
}

#[test]
fn struct_log_limit_counts_json_bytes() {
    let steps = [op::PUSH1, op::PUSH1, op::SSTORE, op::STOP]
        .iter()
        .enumerate()
        .map(|(pc, &op)| test_step(pc, op))
        .collect();
    let builder = GethTraceBuilder::new(vec![CallTraceNode {
        trace: CallTrace { steps, ..Default::default() },
        ..Default::default()
    }]);
    let unlimited = builder.geth_traces(0, Bytes::new(), Default::default()).struct_logs;
    let sizes: Vec<_> = unlimited.iter().map(struct_log_json_len).collect();
    assert_eq!(unlimited.len(), 4);
    // geth logs a step while the bytes written so far do not exceed the limit, so the step that
    // crosses the limit is the last one.
    for limit in [1, sizes[0], sizes[0] + 1, sizes[0] + sizes[1], sizes[..3].iter().sum(), 10_000] {
        let mut written = 0;
        let expected = sizes
            .iter()
            .take_while(|size| {
                let logged = written <= limit;
                written += **size;
                logged
            })
            .count();
        let frame = builder.geth_traces(
            0,
            Bytes::new(),
            GethDefaultTracingOptions { limit: Some(limit as u64), ..Default::default() },
        );
        assert_eq!(frame.struct_logs.len(), expected, "limit {limit}");
    }
}
