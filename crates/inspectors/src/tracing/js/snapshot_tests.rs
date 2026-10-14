//! Compare reconstructed JS step data with snapshots captured before interpreter execution.

use super::*;
use alloy_consensus::TxLegacy;
use alloy_primitives::hex;
use evm2::{
    BaseEvmTypes, Precompiles, SpecId,
    bytecode::Bytecode,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, CacheDB, EmptyDB},
    interpreter::Host,
};
use serde_json::{Value, json};

struct SnapshotInspector {
    js: JsInspector,
    pending: Vec<Value>,
    expected: Vec<Value>,
}

impl Inspector<BaseEvmTypes> for SnapshotInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.pending.push(json!([
            interp.pc(),
            interp.stack().iter().rev().map(ToString::to_string).collect::<Vec<_>>(),
            hex::encode(interp.memory().as_slice()),
            interp.message().depth,
        ]));
        self.js.step(interp);
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.expected.push(self.pending.pop().unwrap());
        self.js.step_end(interp);
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.js.call(interp, message)
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.js.call_end(interp, message, result);
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.js.create(interp, message)
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.js.create_end(interp, message, result);
    }
}

fn compare(code: Bytes, child: Option<Bytes>) -> Vec<Value> {
    let address = Address::repeat_byte(0x11);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &address,
        AccountInfo { code: Some(Bytecode::new_legacy(code.clone())), ..Default::default() },
    );
    if let Some(child) = child {
        db.insert_account_info(
            &Address::repeat_byte(0x22),
            AccountInfo { code: Some(Bytecode::new_legacy(child)), ..Default::default() },
        );
    }
    let script = r#"{
        out: [],
        step: function(log) {
            var stack = [];
            for (var i = 0; i < log.stack.length(); i++) stack.push(log.stack.peek(i).toString());
            this.out.push([log.getPC(), stack, toHex(log.memory.slice(0, log.memory.length())).slice(2), log.getDepth()]);
        },
        fault: function(log) { this.step(log); },
        result: function() { return this.out; }
    }"#;
    let inspector = SnapshotInspector {
        js: JsInspector::new(script.to_string(), Value::Null).unwrap(),
        pending: Vec::new(),
        expected: Vec::new(),
    };
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::CANCUN,
        evm2::env::BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        db,
        Precompiles::base(SpecId::CANCUN),
    );
    evm.set_inspector(inspector);
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            gas_limit: 1_000_000,
            to: TxKind::Call(address),
            ..Default::default()
        }),
        Address::ZERO,
    );
    let res = evm.transact(&tx).unwrap().detach();
    let mut inspector = evm.clear_inspector_as::<SnapshotInspector>().unwrap();
    let block = *evm.block_env();
    let actual = inspector.js.json_result(&res, &tx, &block, evm.database_mut()).unwrap();
    assert_eq!(actual, json!(inspector.expected), "bytecode: {code}");
    inspector.expected
}

#[test]
fn reconstructed_steps_match_all_opcode_bytes() {
    for opcode in 0..=255u8 {
        // Seed nonzero memory, then enough operands for every opcode (including SWAP16).
        let mut code = hex!("60ff60005260aa602052").to_vec();
        for value in 0..20u8 {
            code.extend_from_slice(&[op::PUSH1, value]);
        }
        code.extend_from_slice(&[opcode, op::STOP]);
        compare(code.into(), None);
    }
}

#[test]
fn parent_snapshot_survives_calls_and_return_data() {
    for opcode in [op::CALL, op::CALLCODE, op::DELEGATECALL, op::STATICCALL] {
        for stop in [op::RETURN, op::REVERT] {
            // The child overwrites the parent's existing output region before CALL's step_end.
            let mut parent = hex!("60ff600052").to_vec();
            // Call twice to exercise snapshot reuse at the same depth.
            for _ in 0..2 {
                parent.extend_from_slice(&hex!("6020600060006000"));
                if matches!(opcode, op::CALL | op::CALLCODE) {
                    parent.extend_from_slice(&hex!("6000"));
                }
                parent.push(op::PUSH20);
                parent.extend_from_slice(Address::repeat_byte(0x22).as_slice());
                parent.extend_from_slice(&[op::PUSH2, 0xff, 0xff, opcode, op::POP]);
            }
            parent.push(op::STOP);
            let mut child = hex!("60aa60005260206000").to_vec();
            child.push(stop);
            let steps = compare(parent.into(), Some(child.into()));
            assert!(steps.iter().any(|step| step[3] == 1), "child must execute");
        }
    }
}

#[test]
fn parent_snapshot_survives_create() {
    for opcode in [op::CREATE, op::CREATE2] {
        // Store initcode that writes to its own memory before returning empty runtime code.
        let mut code = hex!("6960aa60005260006000f3600052").to_vec();
        if opcode == op::CREATE2 {
            code.extend_from_slice(&hex!("6001"));
        }
        code.extend_from_slice(&hex!("600a60166000"));
        code.extend_from_slice(&[opcode, op::POP, op::STOP]);
        let steps = compare(code.into(), None);
        assert!(steps.iter().any(|step| step[3] == 1), "initcode must execute");
    }
}
