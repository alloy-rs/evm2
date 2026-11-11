//! Recording where each frame was called from and where it stopped, without steps.

use crate::utils::{AccountInfo, Bytecode, SpecId, TransactTo, TxEnv};
use alloy_primitives::{Address, Bytes, hex};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, ethereum::ethereum_tx_registry, evm::InMemoryDB,
    interpreter::InstrStop,
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

/// Calls `0x43` with all gas and reverts if the call fails.
///
/// `CALL` is at pc 8, the `REVERT` at 14 and the `STOP` at 16.
///
/// `PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH1 0x43 GAS CALL PUSH1 15 JUMPI PUSH0 PUSH0 REVERT JUMPDEST
/// STOP`
const CALL_43: &[u8] = &hex!("5f5f5f5f5f60435af1600f575f5ffd5b00");
/// As [`CALL_43`], calling `0x44`.
const CALL_44: &[u8] = &hex!("5f5f5f5f5f60445af1600f575f5ffd5b00");
/// `PUSH0 PUSH0 REVERT`, with the `REVERT` at pc 2.
const REVERT: &[u8] = &hex!("5f5ffd");
/// `PUSH0 PUSH0 RETURN`, with the `RETURN` at pc 2.
const RETURN: &[u8] = &hex!("5f5ff3");
/// Reverts at pc 7 unless the first calldata word is non-zero, like a Solidity `require`.
///
/// `PUSH0 CALLDATALOAD PUSH1 8 JUMPI PUSH0 PUSH0 REVERT JUMPDEST STOP`
const REQUIRE: &[u8] = &hex!("5f356008575f5ffd5b00");

/// Runs a transaction of `kind` with `data`, with `codes` deployed at `0x42`, `0x43` and so on.
fn inspect(
    kind: TransactTo,
    data: Bytes,
    codes: &[&[u8]],
    config: TracingInspectorConfig,
) -> TracingInspector {
    let mut db = InMemoryDB::default();
    for (i, code) in codes.iter().enumerate() {
        db.insert_account_info(
            &Address::with_last_byte(0x42 + i as u8),
            AccountInfo::default().with_code(Bytecode::new_raw(code.to_vec().into())),
        );
    }
    let mut inspector = TracingInspector::new(config);
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::PRAGUE,
        Default::default(),
        ethereum_tx_registry(SpecId::PRAGUE),
        db,
        Precompiles::base(SpecId::PRAGUE),
    );
    evm.set_inspector(&mut inspector);
    let tx = TxEnv::builder().gas_limit(1_000_000).kind(kind).data(data).build_fill();
    let _ = evm.transact(&tx.envelope()).unwrap();
    drop(evm);
    inspector
}

/// Runs a call to `0x42` with `config`.
fn inspect_call(codes: &[&[u8]], config: TracingInspectorConfig) -> TracingInspector {
    inspect(TransactTo::Call(Address::with_last_byte(0x42)), Bytes::new(), codes, config)
}

/// Runs a call to `0x42` and returns each frame's status, caller pc and exit pc.
fn frames(codes: &[&[u8]]) -> Vec<(InstrStop, Option<usize>, Option<usize>)> {
    frames_of(&inspect_call(codes, TracingInspectorConfig::none().set_frame_pcs(true)))
}

fn frames_of(inspector: &TracingInspector) -> Vec<(InstrStop, Option<usize>, Option<usize>)> {
    inspector
        .traces()
        .nodes()
        .iter()
        .map(|node| (node.trace.status.unwrap(), node.trace.caller_pc, node.trace.exit_pc))
        .collect()
}

#[test]
fn revert_two_calls_deep() {
    assert_eq!(
        frames(&[CALL_43, CALL_44, REVERT]),
        [
            (InstrStop::Revert, None, Some(14)),
            (InstrStop::Revert, Some(8), Some(14)),
            (InstrStop::Revert, Some(8), Some(2)),
        ]
    );
}

#[test]
fn require_one_call_deep() {
    assert_eq!(
        frames(&[CALL_43, REQUIRE]),
        [(InstrStop::Revert, None, Some(14)), (InstrStop::Revert, Some(8), Some(7))]
    );
}

#[test]
fn successful_return_and_stop() {
    assert_eq!(
        frames(&[CALL_43, RETURN]),
        [(InstrStop::Stop, None, Some(16)), (InstrStop::Return, Some(8), Some(2))]
    );
    // Code that runs off its end stops one past its last byte, `PUSH1 1 POP` here.
    assert_eq!(
        frames(&[CALL_43, &hex!("600150")]),
        [(InstrStop::Stop, None, Some(16)), (InstrStop::Stop, Some(8), Some(3))]
    );
}

#[test]
fn call_without_code_has_no_exit_pc() {
    assert_eq!(
        frames(&[CALL_43, &[]]),
        [(InstrStop::Stop, None, Some(16)), (InstrStop::Stop, Some(8), None)]
    );
}

#[test]
fn invalid_and_undefined_opcodes() {
    // `PUSH0 INVALID`.
    assert_eq!(
        frames(&[CALL_43, &hex!("5ffe")]),
        [(InstrStop::Revert, None, Some(14)), (InstrStop::InvalidFEOpcode, Some(8), Some(1))]
    );
    // `PUSH0 0x0c`, an undefined opcode.
    assert_eq!(
        frames(&[CALL_43, &hex!("5f0c")]),
        [(InstrStop::Revert, None, Some(14)), (InstrStop::OpcodeNotFound, Some(8), Some(1))]
    );
}

#[test]
fn out_of_gas_has_no_exit_pc() {
    // `JUMPDEST PUSH0 JUMP` loops until it runs out of gas.
    assert_eq!(
        frames(&[CALL_43, &hex!("5b5f56")]),
        [(InstrStop::Revert, None, Some(14)), (InstrStop::OutOfGas, Some(8), None)]
    );
}

#[test]
fn create_frames() {
    let config = TracingInspectorConfig::none().set_frame_pcs(true);
    let create = |init_code: &[u8]| {
        frames_of(&inspect(TransactTo::Create, init_code.to_vec().into(), &[], config))
    };
    assert_eq!(create(REVERT), [(InstrStop::Revert, None, Some(2))]);

    for (child, status) in [(REVERT, InstrStop::Revert), (RETURN, InstrStop::Return)] {
        // Stores the child's init code in memory, creates it and stops, with `CREATE` at pc 11
        // and `STOP` at 12.
        //
        // `PUSH3 <child> PUSH0 MSTORE PUSH1 3 PUSH1 29 PUSH0 CREATE STOP`
        let init_code = [&[0x62][..], child, &hex!("5f52" "6003" "601d" "5f" "f0" "00")].concat();
        assert_eq!(
            create(&init_code),
            [(InstrStop::Stop, None, Some(12)), (status, Some(11), Some(2))]
        );
    }
}

#[test]
fn exit_pc_is_the_last_step_pc() {
    for codes in [
        &[CALL_43, CALL_44, REVERT][..],
        &[CALL_43, REQUIRE],
        &[CALL_43, RETURN],
        &[CALL_43, &hex!("5ffe")],
    ] {
        let inspector = inspect_call(codes, TracingInspectorConfig::all());
        assert_eq!(frames_of(&inspector), frames(codes));
        for node in inspector.traces().nodes() {
            assert_eq!(node.trace.exit_pc, node.trace.steps.last().map(|step| step.pc));
        }
    }
}

#[test]
fn not_recorded_by_default() {
    for config in [
        TracingInspectorConfig::none(),
        TracingInspectorConfig::default_parity(),
        TracingInspectorConfig::default_geth(),
    ] {
        let inspector = inspect_call(&[CALL_43, REVERT], config);
        for node in inspector.traces().nodes() {
            assert_eq!((node.trace.caller_pc, node.trace.exit_pc), (None, None));
        }
    }
}
