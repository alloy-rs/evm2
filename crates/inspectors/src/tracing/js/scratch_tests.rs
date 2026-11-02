use super::*;
use alloy_consensus::{TxEip1559, transaction::Recovered};
use alloy_primitives::{Address, TxKind, U256, address};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, CacheDB, EmptyDB},
    interpreter::Host,
};
use std::str::FromStr;

#[test]
fn scratch() {
    let Ok(path) = std::env::var("SCEN") else { return };
    let scens: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut out = serde_json::Map::new();
    for s in scens {
        let name = s["name"].as_str().unwrap().to_string();
        let r = run(&s);
        out.insert(name, r);
    }
    std::fs::write(std::env::var("OUT").unwrap(), serde_json::to_string_pretty(&out).unwrap())
        .unwrap();
}

fn run(s: &serde_json::Value) -> serde_json::Value {
    let sender = address!("a94f5374fce5edbc8e2a8697c15331677e6ebf0b");
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &sender,
        AccountInfo { balance: U256::from(1e18), ..Default::default() },
    );
    for (addr, acc) in s["accounts"].as_object().unwrap() {
        let addr = Address::from_str(addr).unwrap();
        let code = alloy_primitives::hex::decode(acc["code"].as_str().unwrap_or("0x")).unwrap();
        let balance = U256::from_str(acc["balance"].as_str().unwrap_or("0x0")).unwrap();
        db.insert_account_info(
            &addr,
            AccountInfo {
                balance,
                code: Some(Bytecode::new_raw(code.into())),
                ..Default::default()
            },
        );
        if let Some(st) = acc["storage"].as_object() {
            for (k, v) in st {
                db.insert_account_storage(
                    &addr,
                    &U256::from_str(k).unwrap(),
                    &U256::from_str(v.as_str().unwrap()).unwrap(),
                );
            }
        }
    }
    let config = if s["config"].is_null() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(s["config"].as_str().unwrap()).unwrap()
    };
    let insp = match JsInspector::new(s["tracer"].as_str().unwrap().to_string(), config) {
        Ok(i) => i,
        Err(e) => return serde_json::json!({"error": e.to_string()}),
    };
    let spec = match s["fork"].as_str().unwrap_or("Cancun") {
        "Amsterdam" => SpecId::AMSTERDAM,
        _ => SpecId::CANCUN,
    };
    let mut evm = Evm::<BaseEvmTypes>::new(
        spec,
        evm2::env::BlockEnvExt {
            basefee: U256::from(5_000_000_000u64),
            number: U256::from(1),
            timestamp: U256::from(1000),
            gas_limit: U256::from(0x1000000000u64),
            beneficiary: address!("2adc25665018aa1fe0e6bc666dac8fc2697ff9ba"),
            ..Default::default()
        },
        ethereum_tx_registry(spec),
        db,
        Precompiles::base(spec),
    );
    evm.set_inspector(insp);
    let to = Address::from_str(s["to"].as_str().unwrap()).unwrap();
    let tx = Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: 1,
            gas_limit: s["gas"].as_u64().unwrap(),
            max_fee_per_gas: 10_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call(to),
            ..Default::default()
        }),
        sender,
    );
    let res = evm.transact(&tx).unwrap().detach();
    let mut inspector = evm.clear_inspector_as::<JsInspector>().unwrap();
    let block = *evm.block_env();
    match inspector.json_result(&res, &tx, &block, evm.database_mut()) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"error": e.to_string()}),
    }
}
