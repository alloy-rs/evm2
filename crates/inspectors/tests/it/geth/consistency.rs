//! Multiplexed tracer configuration and flat-call root gas.

use super::*;
use serde_json::{Value, json};

#[test]
fn mux_config_matches_standalone() {
    for tracer in ["4byteTracer", "noopTracer", "stateGasTracer"] {
        for options in [
            json!({"tracer":tracer, "tracerConfig":"ignored"}),
            json!({"tracer":"muxTracer", "tracerConfig":{tracer:"ignored"}}),
        ] {
            assert!(DebugInspector::new(serde_json::from_value(options).unwrap()).is_ok());
        }
    }
    for tracer in ["callTracer", "prestateTracer", "flatCallTracer"] {
        for (config, valid) in [(Value::Null, true), (json!("invalid"), false)] {
            let options = json!({"tracer":"muxTracer", "tracerConfig":{tracer:config}});
            assert_eq!(
                DebugInspector::new(serde_json::from_value(options).unwrap()).is_ok(),
                valid
            );
        }
    }
}

#[test]
fn flat_call_root_gas_matches_mux() {
    let trace = |options| {
        let target = Address::repeat_byte(0xaa);
        let mut db = CacheDB::<EmptyDB>::default();
        db.insert_account_info(
            &target,
            AccountInfo {
                code: Some(Bytecode::new_raw(hex!("600160005200").into())),
                ..Default::default()
            },
        );
        let inspector = DebugInspector::new(serde_json::from_value(options).unwrap()).unwrap();
        let mut evm = Context::mainnet().with_db(db).build_mainnet_with_inspector(inspector);
        let tx = TxEnv { gas_limit: 1_000_000, kind: TxKind::Call(target), ..Default::default() };
        let result = evm.inspect_tx(tx.clone()).unwrap();
        assert!(result.result.is_success());
        let (ctx, inspector) = evm.ctx_inspector();
        let block = *ctx.block();
        let output = inspector
            .get_result(None, &tx.envelope(), &block, &result.tx_result, ctx.db_mut())
            .unwrap();
        (output, result.result.tx_gas_used())
    };
    let (standalone, gas_used) = trace(json!({"tracer":"flatCallTracer"}));
    let (mux, _) = trace(json!({"tracer":"muxTracer", "tracerConfig":{"flatCallTracer":null}}));
    assert_eq!(
        serde_json::to_value(mux).unwrap()["flatCallTracer"],
        serde_json::to_value(&standalone).unwrap()
    );
    let GethTrace::FlatCallTracer(traces) = standalone else { panic!("expected flat-call trace") };
    assert_eq!(traces[0].trace.result.as_ref().unwrap().gas_used(), gas_used);
}
