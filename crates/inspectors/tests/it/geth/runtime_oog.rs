//! Regressions for the root frame of a transaction that runs out of gas before execution.

use super::*;
use alloy_rpc_types_trace::geth::{CallFrame, CallLogFrame};
use evm2::interpreter::InstrStop;
use evm2_inspectors::tracing::TracingInspector;

#[test]
fn runtime_oog_call_frames_and_reuse() {
    let caller = address!("1000000000000000000000000000000000000001");
    let recipient = address!("1000000000000000000000000000000000000002");
    for mux in [false, true] {
        for only_top_call in [false, true] {
            let config = serde_json::json!({ "onlyTopCall": only_top_call, "withLog": true });
            let options = if mux {
                serde_json::json!({"tracer": "muxTracer", "tracerConfig": {"callTracer": config}})
            } else {
                serde_json::json!({"tracer": "callTracer", "tracerConfig": config})
            };
            let mut inspector =
                DebugInspector::new(serde_json::from_value(options).unwrap()).unwrap();
            let context = Context::mainnet()
                .with_db(CacheDB::<EmptyDB>::default())
                .modify_cfg_chained(|cfg| cfg.spec = SpecId::AMSTERDAM)
                .modify_db_chained(|db| {
                    db.insert_account_info(
                        &caller,
                        AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
                    );
                });
            let mut evm = context.build_mainnet().with_inspector(&mut inspector);

            // Alternate halted and executed roots to catch stale state across fuse(). The
            // successful empty-code call also uses its full gas limit without executing opcodes.
            for (gas_limit, input, kind, halted) in [
                (200_000, Bytes::new(), TxKind::Call(recipient), true),
                (204_600, Bytes::new(), TxKind::Call(recipient), false),
                (200_000, hex!("01020304").into(), TxKind::Call(recipient), true),
                (200_000, hex!("00").into(), TxKind::Create, true),
                (204_600, Bytes::new(), TxKind::Call(recipient), false),
            ] {
                let res = evm
                    .inspect_tx(TxEnv {
                        caller,
                        kind,
                        value: U256::ONE,
                        data: input.clone(),
                        gas_limit,
                        ..Default::default()
                    })
                    .unwrap();
                let result = &res.tx_result.result;
                assert_eq!(result.status, !halted);
                assert_eq!(result.tx_gas_used(), gas_limit);
                let (ctx, inspector) = evm.ctx_inspector();
                let tx = ctx.tx().envelope();
                let block = *ctx.block();
                let trace =
                    inspector.get_result(None, &tx, &block, &res.tx_result, ctx.db_mut()).unwrap();
                // Result construction is idempotent.
                let again =
                    inspector.get_result(None, &tx, &block, &res.tx_result, ctx.db_mut()).unwrap();
                assert_eq!(
                    serde_json::to_value(&trace).unwrap(),
                    serde_json::to_value(again).unwrap()
                );
                let json = serde_json::to_value(trace).unwrap();
                let frame = if mux { json["callTracer"].clone() } else { json };
                let expected = CallFrame {
                    from: caller,
                    to: (!kind.is_create()).then_some(recipient),
                    typ: if kind.is_create() { "CREATE" } else { "CALL" }.into(),
                    value: Some(U256::ONE),
                    input,
                    gas: U256::from(gas_limit),
                    gas_used: U256::from(gas_limit),
                    error: halted.then(|| "out of gas".into()),
                    logs: result
                        .logs
                        .iter()
                        .enumerate()
                        .map(|(index, log)| CallLogFrame {
                            address: Some(log.address),
                            topics: Some(log.topics().to_vec()),
                            data: Some(log.data.data.clone()),
                            position: Some(0),
                            index: Some(index as u64),
                        })
                        .collect(),
                    execution_gas_used: Some(U256::from(result.execution_gas_spent())),
                    state_gas_used: Some(U256::from(result.state_gas_spent())),
                    gas_refund: Some(U256::from(result.final_refunded())),
                    ..Default::default()
                };
                assert_eq!(frame, serde_json::to_value(expected).unwrap());
                inspector.fuse().unwrap();
            }
        }
    }
}

#[test]
fn runtime_oog_frame_reaches_four_byte_tracer() {
    let caller = address!("1000000000000000000000000000000000000001");
    let recipient = address!("1000000000000000000000000000000000000002");
    let options = serde_json::json!({"tracer": "4byteTracer"});
    let mut inspector = DebugInspector::new(serde_json::from_value(options).unwrap()).unwrap();
    let context = Context::mainnet()
        .with_db(CacheDB::<EmptyDB>::default())
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::AMSTERDAM)
        .modify_db_chained(|db| {
            db.insert_account_info(
                &caller,
                AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
            );
        });
    let mut evm = context.build_mainnet().with_inspector(&mut inspector);
    // The gas covers the intrinsic cost but not the new account's state gas.
    let res = evm
        .inspect_tx(TxEnv {
            caller,
            kind: TxKind::Call(recipient),
            value: U256::ONE,
            data: hex!("0102030405").into(),
            gas_limit: 200_000,
            ..Default::default()
        })
        .unwrap();
    assert!(!res.tx_result.result.status);
    let (ctx, inspector) = evm.ctx_inspector();
    let tx = ctx.tx().envelope();
    let block = *ctx.block();
    let trace = inspector.get_result(None, &tx, &block, &res.tx_result, ctx.db_mut()).unwrap();
    assert_eq!(serde_json::to_value(trace).unwrap(), serde_json::json!({"0x01020304-1": 1}));
}

#[test]
fn runtime_oog_root_reports_the_execution_budget() {
    let caller = address!("1000000000000000000000000000000000000001");
    let recipient = address!("1000000000000000000000000000000000000002");
    let context = Context::mainnet()
        .with_db(CacheDB::<EmptyDB>::default())
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::AMSTERDAM)
        .modify_db_chained(|db| {
            db.insert_account_info(
                &caller,
                AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
            );
        });
    let mut evm = context.build_mainnet_with_inspector(TracingInspector::new(Default::default()));
    evm.inspect_tx(TxEnv {
        caller,
        kind: TxKind::Call(recipient),
        value: U256::ONE,
        gas_limit: 200_000,
        ..Default::default()
    })
    .unwrap();
    let nodes = evm.inspector.traces().nodes();
    assert_eq!(nodes.len(), 1);
    let root = &nodes[0].trace;
    assert_eq!(root.status, Some(InstrStop::OutOfGas));
    assert_eq!(root.address, recipient);
    // The halt burns the whole budget, which is what the first frame was given.
    assert!(root.gas_limit < 200_000);
    assert_eq!(root.gas_used, root.gas_limit);
}
