//! Steps that an inspector stops in `step` never receive `step_end`.

use crate::utils::{AccountInfo, Bytecode, Context, TransactTo, TxEnv};
use alloy_primitives::Address;
use evm2::{
    BaseEvmTypes, Inspector,
    evm::InMemoryDB,
    interpreter::{
        InstrStop, Interpreter,
        opcode::{OpCode, op},
    },
};
use evm2_inspectors::{
    opcode::OpcodeGasInspector,
    tracing::{TracingInspector, TracingInspectorConfig},
};

const PARENT: Address = Address::with_last_byte(0x22);
const CHILD: Address = Address::with_last_byte(0x33);

/// Stops the instruction in `step` whenever it is `ADD`, so its `step_end` never runs.
#[derive(Default)]
struct StopAdd {
    enabled: bool,
}

impl Inspector<BaseEvmTypes> for StopAdd {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        if self.enabled && interp.opcode() == op::ADD {
            interp.set_stop(InstrStop::InvalidFEOpcode);
        }
    }
}

/// Runs a parent that calls a child running `PUSH1 1 PUSH1 1 ADD STOP`, then runs `PUSH1 0 POP`.
fn run<I: Inspector<BaseEvmTypes>>(inspector: &mut I) {
    let parent_code = [
        op::PUSH1,
        0,
        op::PUSH1,
        0,
        op::PUSH1,
        0,
        op::PUSH1,
        0,
        op::PUSH1,
        0,
        op::PUSH1,
        0x33,
        op::PUSH2,
        0xff,
        0xff,
        op::CALL,
        op::PUSH1,
        0,
        op::POP,
        op::STOP,
    ];
    let child_code = [op::PUSH1, 1, op::PUSH1, 1, op::ADD, op::STOP];
    let context = Context::mainnet().with_db(InMemoryDB::default()).modify_db_chained(|db| {
        for (address, code) in [(PARENT, &parent_code[..]), (CHILD, &child_code[..])] {
            db.insert_account_info(
                &address,
                AccountInfo {
                    code: Some(Bytecode::new_legacy(code.to_vec().into())),
                    ..Default::default()
                },
            );
        }
    });
    let mut evm = context.build_mainnet().with_inspector(inspector);
    let res = evm
        .inspect_tx(TxEnv {
            caller: Address::ZERO,
            gas_limit: 1_000_000,
            kind: TransactTo::Call(PARENT),
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success(), "{res:#?}");
}

#[test]
fn tracing_discards_steps_stopped_in_step() {
    let steps = |stop| {
        let mut inspector = (
            TracingInspector::new(TracingInspectorConfig::default_geth()),
            StopAdd { enabled: stop },
        );
        run(&mut inspector);
        let nodes = inspector.0.traces().nodes();
        assert_eq!(nodes.len(), 2);
        (nodes[0].trace.steps.clone(), nodes[1].trace.steps.clone())
    };
    let (parent, _) = steps(false);
    let (stopped_parent, stopped_child) = steps(true);

    // The parent's steps do not depend on how its child ended.
    assert_eq!(stopped_parent.len(), parent.len());
    for (stopped, step) in stopped_parent.iter().zip(&parent) {
        assert_eq!(
            (stopped.op, stopped.gas_cost, stopped.status),
            (step.op, step.gas_cost, step.status)
        );
    }
    let call = stopped_parent.iter().find(|step| step.op.get() == op::CALL).unwrap();
    assert_ne!(call.gas_cost, 0);

    // The stopped step never ended.
    let add = stopped_child.last().unwrap();
    assert_eq!(add.op.get(), op::ADD);
    assert_eq!((add.gas_cost, add.status, add.push_stack.as_ref()), (0, None, None));
}

#[test]
fn opcode_gas_discards_steps_stopped_in_step() {
    let mut inspector = (OpcodeGasInspector::new(), StopAdd { enabled: true });
    run(&mut inspector);
    let gas = inspector.0.opcode_gas();
    let call = OpCode::new(op::CALL).unwrap();
    let add = OpCode::new(op::ADD).unwrap();
    let pop = OpCode::new(op::POP).unwrap();
    // The cold CALL costs 2600 regardless of its child.
    assert_eq!(gas.get(&call), Some(&2600));
    assert_eq!(gas.get(&add), None);
    assert_eq!(gas.get(&pop), Some(&2));
}
