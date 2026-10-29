//! Compare reconstructed JS step data with snapshots captured before interpreter execution.

use super::*;
use alloy_consensus::TxLegacy;
use alloy_primitives::hex;
use evm2::{
    BaseEvmTypes, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    constants::CALL_DEPTH_LIMIT,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, CacheDB, EmptyDB},
    interpreter::Host,
};
use serde_json::{Value, json};

struct SnapshotInspector {
    js: JsInspector,
    /// Steps that have started, taken when a child message ends them before `step_end`.
    pending: Vec<Option<Value>>,
    expected: Vec<Value>,
}

impl Inspector<BaseEvmTypes> for SnapshotInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.pending.push(Some(json!([
            interp.pc(),
            interp.stack().iter().rev().map(ToString::to_string).collect::<Vec<_>>(),
            hex::encode(interp.memory().as_slice()),
            interp.message().depth + 1,
        ])));
        self.js.step(interp);
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.expected.extend(self.pending.pop().unwrap());
        self.js.step_end(interp);
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.expected.extend(self.pending.last_mut().and_then(Option::take));
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
        self.expected.extend(self.pending.last_mut().and_then(Option::take));
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

fn compare(code: Bytes, child: Option<Bytes>, initial_call_depth: u16) -> Vec<Value> {
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
        fault: function() {},
        result: function() { return this.out; }
    }"#;
    let inspector = SnapshotInspector {
        js: JsInspector::new(script.to_string(), Value::Null).unwrap(),
        pending: Vec::new(),
        expected: Vec::new(),
    };
    let mut version = Version::new(SpecId::CANCUN);
    version.initial_call_depth = initial_call_depth;
    let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::CANCUN, version),
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
    // Pending steps are indexed relative to the transaction root, not by absolute depth.
    let deepest = inspector.expected.iter().map(|step| step[3].as_u64().unwrap()).max().unwrap();
    assert_eq!(inspector.js.pending_steps.len() as u64, deepest - u64::from(initial_call_depth));
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
        compare(code.into(), None, 0);
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
            let steps = compare(parent.into(), Some(child.into()), 0);
            assert!(steps.iter().any(|step| step[3] == 2), "child must execute");
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
        let steps = compare(code.into(), None, 0);
        assert!(steps.iter().any(|step| step[3] == 2), "initcode must execute");
    }
}

#[test]
fn nested_root_steps_keep_visible_depth() {
    let parent = hex!("5f5f5f5f5f7322222222222222222222222222222222222222225af100");
    let child = Bytes::from_static(&[op::STOP]);
    let root_steps = compare(parent.into(), Some(child.clone()), 0);
    for depth in [0, 7, u16::MAX - CALL_DEPTH_LIMIT - 1] {
        let steps = compare(parent.into(), Some(child.clone()), depth);
        let depths = steps.iter().map(|step| step[3].as_u64().unwrap()).collect::<Vec<_>>();
        let depth = u64::from(depth);
        let mut expected = vec![depth + 1; 8];
        expected.extend([depth + 2, depth + 1]);
        assert_eq!(depths, expected);
        // Apart from the shifted depth, the trace matches the one rooted at depth zero.
        let mut shifted = root_steps.clone();
        for step in &mut shifted {
            step[3] = json!(step[3].as_u64().unwrap() + depth);
        }
        assert_eq!(steps, shifted);
    }
}
