//! Geth Js tracer tests

mod errors;
mod values;

use crate::utils::{
    AccountInfo, Bytecode, CacheDB, Context, EmptyDB, ResultAndState, SpecId, TestDbExt,
    TransactTo, TxEnv, deploy_contract,
};
use alloy_primitives::{Address, U256, address, hex, map::HashMap};
use alloy_rpc_types_trace::geth::{
    GethDebugBuiltInTracerType, GethDebugTracerType, GethDebugTracingOptions, GethTrace,
    mux::MuxConfig,
};
use evm2_inspectors::tracing::{
    DebugInspector, MuxInspector,
    js::{JsInspector, JsInspectorInterrupt},
};
use serde_json::json;

fn js_result(
    insp: &mut JsInspector,
    res: &ResultAndState,
    context: &mut Context,
) -> serde_json::Value {
    let tx = context.tx().envelope();
    let block = *context.block();
    insp.json_result(&res.tx_result, &tx, &block, context.db_mut()).unwrap()
}

#[test]
fn test_geth_jstracer_bigint_compatibility() {
    let code = r#"{
        fault: function() {},
        result: function(ctx) {
            return {
                adjustedValue: ctx.value.add(bigInt('ff', 16)).subtract(1).toString(),
                valueType: typeof ctx.value.valueOf(),
                value: ctx.value.valueOf()
            };
        }
    }"#;
    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let mut evm = Context::mainnet()
        .with_db(CacheDB::new(EmptyDB::default()))
        .build_mainnet()
        .with_inspector(insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: Address::ZERO,
            gas_limit: 100_000,
            kind: TransactTo::Call(address!("0000000000000000000000000000000000001000")),
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success());

    let (context, insp) = evm.ctx_inspector();
    let result = js_result(insp, &res, context);
    assert_eq!(result, json!({"adjustedValue": "254", "valueType": "number", "value": 0}));
}

#[test]
fn test_geth_jstracer_revert() {
    /*
    pragma solidity ^0.8.13;

    contract Foo {
        event Log(address indexed addr, uint256 value);

        function foo() external {
            emit Log(msg.sender, 0);
        }

        function bar() external {
            emit Log(msg.sender, 0);
            require(false, "barbarbar");
        }
    }
    */

    let code = hex!(
        "608060405261023e806100115f395ff3fe608060405234801561000f575f80fd5b5060043610610034575f3560e01c8063c298557814610038578063febb0f7e14610042575b5f80fd5b61004061004c565b005b61004a61009c565b005b3373ffffffffffffffffffffffffffffffffffffffff167ff950957d2407bed19dc99b718b46b4ce6090c05589006dfb86fd22c34865b23e5f6040516100929190610177565b60405180910390a2565b3373ffffffffffffffffffffffffffffffffffffffff167ff950957d2407bed19dc99b718b46b4ce6090c05589006dfb86fd22c34865b23e5f6040516100e29190610177565b60405180910390a25f61012a576040517f08c379a0000000000000000000000000000000000000000000000000000000008152600401610121906101ea565b60405180910390fd5b565b5f819050919050565b5f819050919050565b5f819050919050565b5f61016161015c6101578461012c565b61013e565b610135565b9050919050565b61017181610147565b82525050565b5f60208201905061018a5f830184610168565b92915050565b5f82825260208201905092915050565b7f62617262617262617200000000000000000000000000000000000000000000005f82015250565b5f6101d4600983610190565b91506101df826101a0565b602082019050919050565b5f6020820190508181035f830152610201816101c8565b905091905056fea2646970667358221220e058dc2c4bd629d62405850cc8e08e6bfad0eea187260784445dfe8f3ee0bea564736f6c634300081a0033"
    );
    let deployer = Address::ZERO;

    let mut evm = Context::mainnet()
        .with_db(CacheDB::new(EmptyDB::default()))
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
        .build_mainnet();

    let addr =
        deploy_contract(&mut evm, code.into(), deployer, SpecId::CANCUN).created_address().unwrap();

    let code = r#"
{
    fault: function() {},
    result: function(ctx) { return { error: !!ctx.error }; },
}"#;

    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let mut evm = evm.with_inspector(insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Call(addr),
            data: hex!("c2985578").into(), // call foo
            nonce: 1,
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success());

    let (context, insp) = evm.ctx_inspector();
    let result = js_result(insp, &res, context);

    // successful operation
    assert!(!result["error"].as_bool().unwrap());

    // test with reverted operation
    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let mut evm = evm.with_inspector(insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Call(addr),
            data: hex!("febb0f7e").into(), // call bar
            nonce: 1,
            ..Default::default()
        })
        .unwrap();
    assert!(!res.result.is_success());

    let (context, insp) = evm.ctx_inspector();
    let result = js_result(insp, &res, context);

    // reverted operation
    assert!(result["error"].as_bool().unwrap());
}

// Fix issue https://github.com/paradigmxyz/reth/issues/13089
#[test]
fn test_geth_jstracer_proxy_contract() {
    /*

    contract Token {
        event Transfer(address indexed from, address indexed to, uint256 value);

        function transfer(address to, uint256 amount) public payable {
            emit Transfer(msg.sender, to, amount);
        }
    }

    contract Proxy {
        function transfer(address _contract, address _to, uint256 _amount) external payable {
            (bool success, bytes memory data) =
                _contract.delegatecall(abi.encodeWithSignature("transfer(address,uint256)", _to, _amount));
            require(success, "failed to delegatecall");
        }
    }
    */

    let token_code = hex!(
        "6080604052348015600e575f80fd5b5060dd80601a5f395ff3fe608060405260043610601b575f3560e01c8063a9059cbb14601f575b5f80fd5b602e602a3660046074565b6030565b005b6040518181526001600160a01b0383169033907fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef9060200160405180910390a35050565b5f80604083850312156084575f80fd5b82356001600160a01b03811681146099575f80fd5b94602093909301359350505056fea2646970667358221220d81408f997c5f148e7d6afc66ccc7cda17a38396925363f11993fa885b70729b64736f6c63430008190033"
    );

    let proxy_code = hex!(
        "6080604052348015600e575f80fd5b506101998061001c5f395ff3fe60806040526004361061001d575f3560e01c80631a69523014610021575b5f80fd5b61003461002f366004610120565b610036565b005b6040516104006024820152606560448201525f9081906001600160a01b0384169060640160408051601f198184030181529181526020820180516001600160e01b031663a9059cbb60e01b1790525161008f919061014d565b5f60405180830381855af49150503d805f81146100c7576040519150601f19603f3d011682016040523d82523d5f602084013e6100cc565b606091505b50915091508161011b5760405162461bcd60e51b815260206004820152601660248201527519985a5b1959081d1bc819195b1959d85d1958d85b1b60521b604482015260640160405180910390fd5b505050565b5f60208284031215610130575f80fd5b81356001600160a01b0381168114610146575f80fd5b9392505050565b5f82518060208501845e5f92019182525091905056fea2646970667358221220d7855999519e998c7bcef0432918ca2f5b00228a4058ba259260e327013226f764736f6c63430008190033"
    );

    let deployer = address!("f077b491b355e64048ce21e3a6fc4751eeea77fa");

    let mut evm = Context::mainnet()
        .with_db(CacheDB::new(EmptyDB::default()))
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
        .build_mainnet();

    // Deploy Implementation(Token) contract
    let token_addr =
        deploy_contract(&mut evm, token_code.into(), Address::default(), SpecId::CANCUN)
            .created_address()
            .unwrap();

    // Deploy Proxy contract
    let proxy_addr = deploy_contract(&mut evm, proxy_code.into(), Address::ZERO, SpecId::CANCUN)
        .created_address()
        .unwrap();
    evm.ctx().db_mut().load_account(deployer).info.balance = U256::from(1_000_000);

    // Set input data for ProxyFactory.transfer(address)
    let mut input_data = hex!("1a695230").to_vec(); // keccak256("transfer(address)")[:4]
    input_data.extend_from_slice(&[0u8; 12]); // Pad with zeros
    input_data.extend_from_slice(token_addr.as_slice());
    println!("token {token_addr:?} proxy: {proxy_addr:?}");

    let code = r#"
{
    data: [],
    fault: function(log) {},
    step: function(log) {
        if (log.op.toString().match(/LOG/)) {
            const topic1 = log.stack.peek(2).toString(16);
            const caller = toHex(log.contract.getCaller());
            const token = toHex(log.contract.getAddress());
            const value = log.contract.getValue().toString();
            if (topic1 === "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef") {
                this.data.push({ event: "Transfer", token, caller, value })
            }
        }
    },
    result: function() { return this.data; }
}"#;
    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();

    let mut evm = evm.with_inspector(insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Call(proxy_addr),
            data: input_data.into(),
            value: U256::from(7),
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success());

    let (context, insp) = evm.ctx_inspector();
    let result = js_result(insp, &res, context);
    assert_eq!(
        result,
        json!([{"event": "Transfer", "token": proxy_addr, "caller": deployer, "value": "7"}])
    );
}

#[test]
fn test_geth_jstracer_records_call_step_before_nested_execution() {
    let child = address!("0000000000000000000000000000000000001234");
    let parent = address!("0000000000000000000000000000000000000022");

    let child_code =
        Bytecode::new_legacy([0x60, 1, 0x60, 0x40, 0x52, 0x60, 1, 0x60, 1, 0x01, 0x00].into());
    let parent_code = Bytecode::new_legacy(
        [
            0x60, 1, 0x60, 0, 0x52, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x61, 0x12, 0x34,
            0x61, 0xff, 0xff, 0xf1, 0x00,
        ]
        .into(),
    );

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &child,
                AccountInfo { code: Some(child_code), ..Default::default() },
            );
            db.insert_account_info(
                &parent,
                AccountInfo { code: Some(parent_code), ..Default::default() },
            );
        });

    let code = r#"
{
    ops: [],
    step: function(log) {
        this.ops.push({ op: log.op.toString(), depth: log.getDepth(), mem: log.memory.length() });
    },
    fault: function() {},
    result: function() { return this.ops; }
}"#;
    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let mut evm = context.build_mainnet().with_inspector(insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: Address::ZERO,
            gas_limit: 1_000_000,
            kind: TransactTo::Call(parent),
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success(), "{res:#?}");

    let (context, insp) = evm.ctx_inspector();
    let result = js_result(insp, &res, context);
    let ops = result.as_array().unwrap();
    let call = ops.iter().position(|step| step["op"] == "CALL").expect("missing CALL step");
    let add = ops.iter().position(|step| step["op"] == "ADD").expect("missing child ADD step");
    assert!(call < add, "CALL step should be observed before child execution: {ops:?}");

    let parent_stop = ops
        .iter()
        .find(|step| step["op"] == "STOP" && step["depth"].as_u64() == Some(1))
        .expect("missing parent STOP step");
    assert_eq!(parent_stop["mem"].as_u64(), Some(32));
}

#[test]
fn test_geth_jstracer_call_step_cost_and_depth() {
    let leaf = address!("0000000000000000000000000000000000001111");
    let mid = address!("0000000000000000000000000000000000002222");
    let reverter = address!("0000000000000000000000000000000000003333");
    let main = address!("0000000000000000000000000000000000004444");

    // PUSH1 1, PUSH1 1, ADD, STOP
    let leaf_code = hex!("600160010100");
    // CALL(0x4000, leaf, 0, 0, 0, 0, 0), POP, STOP
    let mid_code = hex!("5f5f5f5f5f611111614000f15000");
    // REVERT(0, 0)
    let reverter_code = hex!("5f5ffd");
    let main_code = hex!(
        // CALL(0xffff, mid, 0, 0, 0, 0, 0), POP
        "5f5f5f5f5f61222261fffff150"
        // CALL(0xffff, 0xdead, 0, 0, 0, 0, 0) to an account without code, POP
        "5f5f5f5f5f61dead61fffff150"
        // STATICCALL(0xffff, reverter, 0, 0, 0, 0), POP
        "5f5f5f5f61333361fffffa50"
        // MSTORE(0, 0x5f5ff3), CREATE(0, 29, 3) with initcode RETURN(0, 0), POP
        "625f5ff35f526003601d5ff050"
        // CREATE2(0, 29, 3, 1), POP
        "60016003601d5ff550"
        // DELEGATECALL(0xffff, leaf, 0, 0, 0, 0), POP
        "5f5f5f5f61111161fffff450"
        // CALLCODE(0xffff, leaf, 0, 0, 0, 0, 0), POP
        "5f5f5f5f5f61111161fffff250"
        // STATICCALL(0xffff, identity precompile, 0, 0, 0, 0), POP
        "5f5f5f5f600461fffffa50"
        // CALL(0xffff, leaf, 1, 0, 0, 0, 0) without the funds to transfer, POP, STOP
        "5f5f5f5f600161111161fffff15000"
    );

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            for (address, code) in [
                (leaf, &leaf_code[..]),
                (mid, &mid_code[..]),
                (reverter, &reverter_code[..]),
                (main, &main_code[..]),
            ] {
                let code = Some(Bytecode::new_legacy(code.to_vec().into()));
                db.insert_account_info(&address, AccountInfo { code, ..Default::default() });
            }
        });

    let code = r#"
{
    events: [],
    step: function(log) {
        var op = log.op.toString();
        if (op.indexOf("CALL") >= 0 || op.indexOf("CREATE") >= 0) {
            this.events.push([op, log.getDepth(), log.getGas(), log.getCost(), log.stack.peek(1).toString(16)]);
        } else if (op == "ADD") {
            this.events.push([op, log.getDepth()]);
        }
    },
    enter: function(frame) { this.events.push(["enter", frame.getType()]); },
    exit: function() { this.events.push(["exit"]); },
    fault: function(log) { this.events.push(["fault", log.getDepth()]); },
    result: function() { return this.events; }
}"#;
    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let mut evm = context.build_mainnet().with_inspector(insp);
    let res = evm
        .inspect_tx(TxEnv {
            caller: Address::ZERO,
            gas_limit: 1_000_000,
            kind: TransactTo::Call(main),
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success(), "{res:#?}");

    let (context, insp) = evm.ctx_inspector();
    let mut events = js_result(insp, &res, context);
    // A call step costs the account access plus any value transfer (9000) plus the gas forwarded
    // to the child: 2600 for a cold account, 100 for a warm one or a precompile.
    let cold_call = 2600 + 0xffff;
    let warm_call = 100 + 0xffff;
    // A create step costs its base, then forwards all but 1/64 of the remaining gas. The base is
    // 32000 plus 2 per initcode word, and CREATE2 adds 6 per word to hash the initcode.
    let mut create_costs = Vec::new();
    for event in events.as_array_mut().unwrap().iter_mut().filter(|event| event[2].is_u64()) {
        let gas = event[2].take().as_u64().unwrap();
        let base = match event[0].as_str() {
            Some("CREATE") => 32002,
            Some("CREATE2") => 32008,
            _ => continue,
        };
        create_costs.push(gas - (gas - base) / 64);
    }
    assert_eq!(
        events,
        json!([
            ["CALL", 1, null, cold_call, "2222"],
            ["enter", "CALL"],
            ["CALL", 2, null, 2600 + 0x4000, "1111"],
            ["enter", "CALL"],
            ["ADD", 3],
            ["exit"],
            ["exit"],
            ["CALL", 1, null, cold_call, "dead"],
            ["enter", "CALL"],
            ["exit"],
            ["STATICCALL", 1, null, cold_call, "3333"],
            ["enter", "STATICCALL"],
            ["fault", 2],
            ["exit"],
            ["CREATE", 1, null, create_costs[0], "1d"],
            ["enter", "CREATE"],
            ["exit"],
            ["CREATE2", 1, null, create_costs[1], "1d"],
            ["enter", "CREATE2"],
            ["exit"],
            ["DELEGATECALL", 1, null, warm_call, "1111"],
            ["enter", "DELEGATECALL"],
            ["ADD", 2],
            ["exit"],
            ["CALLCODE", 1, null, warm_call, "1111"],
            ["enter", "CALLCODE"],
            ["ADD", 2],
            ["exit"],
            ["STATICCALL", 1, null, warm_call, "4"],
            ["enter", "STATICCALL"],
            ["exit"],
            ["CALL", 1, null, 9000 + warm_call, "1111"],
            ["enter", "CALL"],
            ["exit"],
        ])
    );
}

#[test]
fn test_geth_debug_inspector_jstracer() {
    let account = address!("1000000000000000000000000000000000000001");
    let caller = address!("1000000000000000000000000000000000000002");
    let coinbase = address!("1000000000000000000000000000000000000003");

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &caller,
                AccountInfo::default().with_balance(U256::from(1_000_000_000u64)),
            );
            db.insert_account_info(
                &account,
                AccountInfo {
                    code: Some(Bytecode::new_legacy(hex!("6001600100").into())),
                    ..Default::default()
                },
            );
        });
    let context = context
        .modify_block_chained(|block| {
            block.number = U256::from(42);
            block.beneficiary = coinbase;
            block.timestamp = U256::from(1234);
        })
        .modify_tx_chained(|tx| {
            tx.gas_limit = 1_000_000;
            tx.gas_price = 17;
            tx.kind = TransactTo::Call(account);
            tx.value = U256::from(3);
        });

    let code = r#"
{
    count: 0,
    step: function() { this.count += 1; },
    fault: function() {},
    result: function(ctx, db) {
        return {
            count: this.count,
            from: toHex(ctx.from),
            to: toHex(ctx.to),
            exists: db.exists(ctx.to),
            codeLength: db.getCode(ctx.to).length,
            gas: ctx.gas,
            gasPrice: ctx.gasPrice.toString(),
            value: ctx.value.toString(),
            block: ctx.block,
            coinbase: toHex(ctx.coinbase),
            error: !!ctx.error
        };
    }
}"#;
    let mut inspector = DebugInspector::new(GethDebugTracingOptions::js_tracer(code)).unwrap();
    let mut evm = context.build_mainnet().with_inspector(&mut inspector);

    let tx = evm.ctx().tx().clone();
    let res = evm.inspect_tx(TxEnv { caller, ..tx }).unwrap();
    assert!(res.result.is_success());

    let (context, inspector) = evm.ctx_inspector();
    let tx_env = context.tx().envelope();
    let block_env = *context.block();
    let trace =
        inspector.get_result(None, &tx_env, &block_env, &res.tx_result, context.db_mut()).unwrap();

    let GethTrace::JS(result) = trace else { panic!("expected JS trace") };
    assert_eq!(result["count"], json!(3));
    assert_eq!(result["from"], json!(caller));
    assert_eq!(result["to"], json!(account));
    assert_eq!(result["exists"], json!(true));
    assert_eq!(result["codeLength"], json!(5));
    assert_eq!(result["gas"], json!(1_000_000));
    assert_eq!(result["gasPrice"], json!("17"));
    assert_eq!(result["value"], json!("3"));
    assert_eq!(result["block"], json!(42));
    assert_eq!(result["coinbase"], json!(coinbase));
    assert_eq!(result["error"], json!(false));
}

#[test]
fn test_geth_jstracer_get_code_does_not_load_the_account() {
    let target = address!("1000000000000000000000000000000000000001");
    let observed = address!("1000000000000000000000000000000000000002");
    let caller = address!("1000000000000000000000000000000000000003");

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &caller,
                AccountInfo::default().with_balance(U256::from(1_000_000_000u64)),
            );
            db.insert_account_info(
                &target,
                AccountInfo {
                    code: Some(Bytecode::new_legacy(hex!("00").into())),
                    ..Default::default()
                },
            );
            db.insert_account_info(
                &observed,
                AccountInfo {
                    code: Some(Bytecode::new_legacy(hex!("6001600100").into())),
                    ..Default::default()
                },
            );
        });
    let context = context.modify_tx_chained(|tx| {
        tx.gas_limit = 1_000_000;
        tx.gas_price = 1;
        tx.kind = TransactTo::Call(target);
    });

    let code = r#"
{
    codeLength: -1,
    step: function(log, db) {
        this.codeLength = db.getCode(toAddress("1000000000000000000000000000000000000002")).length;
    },
    fault: function() {},
    result: function() { return this.codeLength; }
}"#;
    let mut inspector = DebugInspector::new(GethDebugTracingOptions::js_tracer(code)).unwrap();
    let mut evm = context.build_mainnet().with_inspector(&mut inspector);

    let tx = evm.ctx().tx().clone();
    let res = evm.inspect_tx(TxEnv { caller, ..tx }).unwrap();
    assert!(res.result.is_success());

    let (context, inspector) = evm.ctx_inspector();
    let tx_env = context.tx().envelope();
    let block_env = *context.block();
    let trace =
        inspector.get_result(None, &tx_env, &block_env, &res.tx_result, context.db_mut()).unwrap();
    let GethTrace::JS(result) = trace else { panic!("expected JS trace") };
    assert_eq!(result, json!(5));

    // The transaction loads its caller and target. The code read does not add `observed`.
    let pending = &res.tx_result.pending_state;
    assert!(pending.account_changes_for(&observed).is_none());
    assert!(pending.account_changes_for(&target).is_some());
}

#[test]
fn mux_runs_a_js_tracer_next_to_a_built_in_tracer() {
    let code = "{ steps: 0, step: function() { this.steps++; }, fault: function() {}, \
                result: function() { return this.steps; } }";
    let contract = address!("0000000000000000000000000000000000000042");
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &contract,
        AccountInfo::default().with_code(Bytecode::new_raw(hex!("5f5f00").into())),
    );
    let js_key = GethDebugTracerType::JsTracer(code.to_string());
    let call_key = GethDebugTracerType::BuiltInTracer(GethDebugBuiltInTracerType::CallTracer);
    let config = MuxConfig(HashMap::from_iter([(js_key.clone(), None), (call_key.clone(), None)]));
    let mut evm = Context::mainnet()
        .with_db(db)
        .build_mainnet_with_inspector(MuxInspector::try_from_config(config).unwrap());
    let res = evm
        .inspect_tx(TxEnv {
            gas_limit: 1_000_000,
            kind: TransactTo::Call(contract),
            ..Default::default()
        })
        .unwrap();

    let (ctx, inspector) = evm.ctx_inspector();
    let (tx, block) = (ctx.tx().envelope(), *ctx.block());
    let frame =
        inspector.try_into_mux_frame(&res.tx_result, &tx, &block, None, ctx.db_mut()).unwrap();

    let GethTrace::JS(steps) = &frame.0[&js_key] else { panic!("expected a JS result") };
    assert_eq!(steps.as_u64(), Some(3));
    assert!(matches!(frame.0[&call_key], GethTrace::CallTracer(_)));
}

#[test]
fn mux_interrupts_its_js_tracers() {
    let code = "{ fault: function() {}, result: function() { return 1; } }";
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&Address::ZERO, AccountInfo::default());
    let config =
        MuxConfig(HashMap::from_iter([(GethDebugTracerType::JsTracer(code.into()), None)]));
    let mut evm = Context::mainnet()
        .with_db(db)
        .build_mainnet_with_inspector(MuxInspector::try_from_config(config).unwrap());
    let res = evm
        .inspect_tx(TxEnv {
            gas_limit: 1_000_000,
            kind: TransactTo::Call(Address::ZERO),
            ..Default::default()
        })
        .unwrap();

    let interrupt = JsInspectorInterrupt::new();
    let (ctx, inspector) = evm.ctx_inspector();
    inspector.set_js_interrupt(&interrupt);
    let (tx, block) = (ctx.tx().envelope(), *ctx.block());
    assert!(inspector.try_into_mux_frame(&res.tx_result, &tx, &block, None, ctx.db_mut()).is_ok());

    interrupt.interrupt();
    assert!(inspector.try_into_mux_frame(&res.tx_result, &tx, &block, None, ctx.db_mut()).is_err());
}
