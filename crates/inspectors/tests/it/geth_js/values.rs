//! SELFDESTRUCT frame values and nested execution refunds.

use super::*;

fn trace_tx(code: &str, db: CacheDB<EmptyDB>, tx: TxEnv) -> serde_json::Value {
    let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
        .with_db(db)
        .build_mainnet_with_inspector(insp);
    let res = evm.inspect_tx(tx).unwrap();
    let (ctx, inspector) = evm.ctx_inspector();
    js_result(inspector, &res, ctx)
}

fn funded_db() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &Address::ZERO,
        AccountInfo { balance: U256::from(1e18), ..Default::default() },
    );
    db
}

#[test]
fn test_selfdestruct_frame_describes_the_destruction() {
    let contract = Address::repeat_byte(0xaa);
    let beneficiary = Address::repeat_byte(0xcc);
    let mut db = funded_db();
    // PUSH20 <beneficiary>, SELFDESTRUCT
    let mut code = vec![0x73];
    code.extend_from_slice(beneficiary.as_slice());
    code.push(0xff);
    db.insert_account_info(
        &contract,
        AccountInfo {
            balance: U256::from(5),
            code: Some(Bytecode::new_legacy(code.into())),
            ..Default::default()
        },
    );

    let tracer = r#"{f:[],step:function(){},fault:function(){},enter:function(fr){this.f.push([fr.getType(),toHex(fr.getFrom()),toHex(fr.getTo()),fr.getValue().toString(),fr.getGas(),toHex(fr.getInput())])},exit:function(){},result:function(){return this.f}}"#;
    let res = trace_tx(
        tracer,
        db,
        TxEnv { gas_limit: 1_000_000, kind: TransactTo::Call(contract), ..Default::default() },
    );
    assert_eq!(
        res,
        json!([[
            "SELFDESTRUCT",
            format!("{contract:#x}"),
            format!("{beneficiary:#x}"),
            "5",
            0,
            "0x"
        ]])
    );
}

// Slot 0 starts at 1 in every account so SSTORE-to-zero earns a refund.
fn refund_steps(accounts: &[(Address, Vec<u8>)], outer: Address) -> serde_json::Value {
    let mut db = funded_db();
    for (addr, code) in accounts {
        db.insert_account_info(
            addr,
            AccountInfo {
                code: Some(Bytecode::new_legacy(code.clone().into())),
                ..Default::default()
            },
        );
        db.insert_account_storage(addr, &U256::ZERO, &U256::ONE);
    }
    let code = r#"{r:[],step:function(log){if(['SSTORE','CALL','DELEGATECALL','CREATE','STOP'].includes(log.op.toString()))this.r.push([log.getDepth(),log.op.toString(),log.getRefund()])},fault:function(){},result:function(){return this.r}}"#;
    trace_tx(
        code,
        db,
        TxEnv { gas_limit: 1_000_000, kind: TransactTo::Call(outer), ..Default::default() },
    )
}

/// `SSTORE slot0 = 0`, then `op` (CALL or DELEGATECALL) to `target`, then `STOP`.
fn clear_then_call(op: u8, target: Address) -> Vec<u8> {
    let mut code = hex!("6000600055").to_vec();
    code.extend_from_slice(&hex!("6000600060006000"));
    if op == 0xf1 {
        code.extend_from_slice(&hex!("6000")); // value
    }
    code.push(0x73);
    code.extend_from_slice(target.as_slice());
    code.extend_from_slice(&hex!("61ffff"));
    code.push(op);
    code.push(0x00);
    code
}

#[test]
fn refund_includes_ancestors_and_negative_child_counters() {
    let (outer, inner, restorer) =
        (Address::repeat_byte(0xaa), Address::repeat_byte(0xbb), Address::repeat_byte(0xcc));
    // Each caller clears slot 0; the delegate restores it, earning a negative local refund.
    let steps = refund_steps(
        &[
            (outer, clear_then_call(0xf1, inner)),
            (inner, clear_then_call(0xf4, restorer)),
            (restorer, hex!("600160005500").to_vec()),
        ],
        outer,
    );
    assert_eq!(
        steps,
        json!([
            [0, "SSTORE", 4800],
            [0, "CALL", 4800],
            [1, "SSTORE", 9600],
            [1, "DELEGATECALL", 9600],
            [2, "SSTORE", 7600],
            [2, "STOP", 7600],
            [1, "STOP", 7600],
            [0, "STOP", 7600]
        ])
    );
}

#[test]
fn test_get_refund_drops_a_reverted_frame() {
    let (outer, inner) = (Address::repeat_byte(0xaa), Address::repeat_byte(0xbb));
    let steps = refund_steps(
        &[(outer, clear_then_call(0xf1, inner)), (inner, hex!("600060005560006000fd").to_vec())],
        outer,
    );
    assert_eq!(
        steps,
        json!([[0, "SSTORE", 4800], [0, "CALL", 4800], [1, "SSTORE", 9600], [0, "STOP", 4800]])
    );
}

#[test]
fn test_get_refund_at_create_excludes_the_unfinished_child() {
    let outer = Address::repeat_byte(0xaa);
    // SSTORE slot0 = 0 earns 4800, then CREATE initcode that sets and resets its own slot 1.
    let steps = refund_steps(
        &[(outer, hex!("60006000556a6001600155600060015500600052600b60156000f000").to_vec())],
        outer,
    );
    assert_eq!(
        steps,
        json!([
            [0, "SSTORE", 4800],
            [0, "CREATE", 4800],
            [1, "SSTORE", 4800],
            [1, "SSTORE", 24700],
            [1, "STOP", 24700],
            [0, "STOP", 24700]
        ])
    );
}
