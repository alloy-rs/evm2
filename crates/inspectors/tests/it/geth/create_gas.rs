//! Creation opcode costs in structured logs and the other trace formats.

use super::*;
use evm2_inspectors::tracing::StackSnapshotType;

fn inspect(
    code: &[u8],
    child: &[u8],
    gas: u64,
    config: TracingInspectorConfig,
) -> (TracingInspector, u64, bool) {
    let parent = address!("00000000000000000000000000000000000b0b00");
    let mut db = CacheDB::<EmptyDB>::default();
    for (address, code) in [(parent, code), (Address::with_last_byte(0x43), child)] {
        db.insert_account_info(
            &address,
            AccountInfo {
                code: Some(Bytecode::new_raw(Bytes::copy_from_slice(code))),
                ..Default::default()
            },
        );
    }
    let mut evm = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::OSAKA)
        .build_mainnet_with_inspector(TracingInspector::new(config));
    let result = evm
        .inspect_tx(TxEnv { gas_limit: gas, kind: TransactTo::Call(parent), ..Default::default() })
        .unwrap();
    let spent = result.result.tx_gas_used();
    let success = result.result.is_success();
    (evm.into_inspector(), spent, success)
}

#[test]
fn structured_create_cost_excludes_forwarded_gas() {
    for opcode in [op::CREATE, op::CREATE2] {
        for (init, gas, spent, success) in [
            (&[][..], 200_000, 0, true),
            (&hex!("00")[..], 200_000, 0, true),
            (&hex!("60205ff3")[..], 200_000, 6_408, true),
            (&hex!("5f5ffd")[..], 200_000, 4, false),
            (&hex!("fe")[..], 200_000, u64::MAX, false),
            (&hex!("5f5f55")[..], 54_000, u64::MAX, false),
        ] {
            let mut code = Vec::new();
            if !init.is_empty() {
                code.push(op::PUSH0 + init.len() as u8);
                code.extend_from_slice(init);
                code.extend_from_slice(&[op::PUSH0, op::MSTORE]);
            }
            if opcode == op::CREATE2 {
                code.push(op::PUSH0);
            }
            code.extend_from_slice(&[
                op::PUSH1,
                init.len() as u8,
                op::PUSH1,
                if init.is_empty() { 0 } else { 32 - init.len() as u8 },
                op::PUSH0,
                opcode,
                op::STOP,
            ]);
            let cost = 32_000
                + if init.is_empty() {
                    0
                } else if opcode == op::CREATE2 {
                    8
                } else {
                    2
                };
            for config in [
                TracingInspectorConfig::default_geth(),
                TracingInspectorConfig::all(),
                TracingInspectorConfig::default_geth().set_stack_snapshots(StackSnapshotType::None),
            ] {
                let (inspector, gas_used, root_success) = inspect(&code, &[], gas, config);
                assert!(root_success);
                let nodes = inspector.traces().nodes();
                let parent = &nodes[0].trace;
                let child = &nodes[1].trace;
                assert_eq!(child.success, success);
                let spent = spent.min(child.gas_limit);
                assert_eq!(child.gas_used, spent);
                let step = parent.steps.iter().find(|step| step.op.get() == opcode).unwrap();
                let available = step.gas_remaining - cost;
                assert_eq!(child.gas_limit, available - available / 64);
                assert_eq!(step.gas_cost, cost + child.gas_limit);
                assert_eq!(gas_used, gas - step.gas_remaining + cost + spent);

                let builder = inspector.geth_builder();
                let frame = builder.geth_traces(
                    gas_used,
                    Bytes::new(),
                    GethDefaultTracingOptions::default(),
                );
                let name = if opcode == op::CREATE { "CREATE" } else { "CREATE2" };
                let log = frame.struct_logs.iter().find(|log| log.op == name).unwrap();
                assert_eq!(log.gas_cost, cost, "{name}, init {init:?}");
                assert_eq!(log.gas, step.gas_remaining);
                assert_eq!(frame.gas, gas_used);
                assert!(!frame.failed);
                assert_eq!(frame.struct_logs.last().unwrap().gas, available - spent);
                let call = builder.geth_call_traces(CallConfig::default(), gas_used);
                assert_eq!(call.gas_used, U256::from(gas_used));
                assert_eq!(call.calls[0].gas, U256::from(child.gas_limit));
                assert_eq!(call.calls[0].gas_used, U256::from(spent));

                let parity = inspector.clone().into_parity_builder().vm_trace();
                let create = &parity.ops[parity.ops.len() - 2];
                assert_eq!(create.cost, cost + child.gas_limit);
                assert_eq!(create.ex.as_ref().unwrap().used, available - spent);
            }
        }
    }
}

#[test]
fn structured_create_cost_includes_memory_and_initcode_charges() {
    for (code, cost, gas_used) in [
        (&hex!("5f5f5ff000")[..], 32_000, 53_006),
        (&hex!("5f5f5f5ff500")[..], 32_000, 53_008),
        (&hex!("60215f5ff000")[..], 32_010, 53_017),
        (&hex!("5f60215f5ff500")[..], 32_022, 53_031),
        (&hex!("5f5f6001f000")[..], 32_000, 53_007),
        (&hex!("5f5f5f6001f500")[..], 32_000, 53_009),
    ] {
        let (inspector, spent, success) =
            inspect(code, &[], 200_000, TracingInspectorConfig::all());
        assert!(success);
        assert_eq!(spent, gas_used);
        let frame = inspector.geth_builder().geth_traces(
            spent,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        let create = frame
            .struct_logs
            .iter()
            .find(|log| matches!(log.op.as_ref(), "CREATE" | "CREATE2"))
            .unwrap();
        assert_eq!(create.gas_cost, cost);
        let child_gas = inspector.traces().nodes()[1].trace.gas_limit;
        let parity = inspector.into_parity_builder().vm_trace();
        assert_eq!(parity.ops[parity.ops.len() - 2].cost, cost + child_gas);
    }
}

#[test]
fn structured_create_without_child_keeps_attempted_cost() {
    for code in [&hex!("5f5f5ff000")[..], &hex!("5f5f5f5ff500")[..]] {
        let (inspector, gas_used, success) =
            inspect(code, &[], 53_000, TracingInspectorConfig::default_geth());
        assert!(!success);
        assert_eq!(gas_used, 53_000);
        assert_eq!(inspector.traces().nodes().len(), 1);
        let frame = inspector.geth_builder().geth_traces(
            gas_used,
            Bytes::new(),
            GethDefaultTracingOptions::default(),
        );
        assert!(frame.failed);
        assert_eq!(frame.struct_logs.last().unwrap().gas_cost, 32_000);
    }
}

#[test]
fn structured_call_cost_still_includes_forwarded_gas() {
    for (prefix, opcode) in
        [("5f5f5f5f5f", "f1"), ("5f5f5f5f5f", "f2"), ("5f5f5f5f", "f4"), ("5f5f5f5f", "fa")]
    {
        for child in [&hex!("00")[..], &hex!("5f5ffd")[..], &hex!("fe")[..], &[][..]] {
            let code = hex::decode(format!("{prefix}604361ffff{opcode}00")).unwrap();
            let (inspector, gas_used, success) =
                inspect(&code, child, 200_000, TracingInspectorConfig::all());
            assert!(success);
            let frame = inspector.geth_builder().geth_traces(
                gas_used,
                Bytes::new(),
                GethDefaultTracingOptions::default(),
            );
            let call = frame
                .struct_logs
                .iter()
                .find(|log| {
                    matches!(log.op.as_ref(), "CALL" | "CALLCODE" | "DELEGATECALL" | "STATICCALL")
                })
                .unwrap();
            assert_eq!(call.gas_cost, 68_135);
            let parity = inspector.into_parity_builder().vm_trace();
            assert_eq!(parity.ops[parity.ops.len() - 2].cost, 68_135);
        }
    }
}
