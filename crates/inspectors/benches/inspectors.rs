#![allow(missing_docs)]

//! Per-step and per-frame recording cost of the tracing inspectors, and the cost of building
//! geth traces from a recording.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256, address};
use alloy_rpc_types_trace::geth::{CallConfig, GethDefaultTracingOptions};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use evm2::{
    BaseEvmTypes, Evm, Inspector, Precompiles, SpecId,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{RecoveredTxEnvelope, TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
};
use evm2_inspectors::{
    opcode::OpcodeGasInspector,
    tracing::{TracingInspector, TracingInspectorConfig},
};

const CALLER: Address = address!("0x1000000000000000000000000000000000000001");
const TARGET: Address = address!("0x2000000000000000000000000000000000000002");
const CHILD: Address = address!("0x000000000000000000000000000000000000beef");

/// Loops 1000 times over memory, storage and a STATICCALL to [`CHILD`].
const LOOP: &[u8] = &[
    0x61, 0x03, 0xe8, // PUSH2 1000
    0x5b, // JUMPDEST
    0x80, 0x5f, 0x52, // DUP1 PUSH0 MSTORE
    0x5f, 0x51, 0x50, // PUSH0 MLOAD POP
    0x80, 0x60, 0x07, 0x16, // DUP1 PUSH1 7 AND
    0x80, 0x54, 0x50, // DUP1 SLOAD POP
    0x81, 0x90, 0x55, // DUP2 SWAP1 SSTORE
    0x5f, 0x5f, 0x5f, 0x5f, 0x61, 0xbe, 0xef, 0x5a, 0xfa, 0x50, // STATICCALL(CHILD) POP
    0x60, 0x01, 0x90, 0x03, // PUSH1 1 SWAP1 SUB
    0x80, 0x60, 0x03, 0x57, // DUP1 PUSH1 3 JUMPI
    0x00, // STOP
];

/// PUSH1 1 PUSH1 2 ADD POP STOP.
const CHILD_CODE: &[u8] = &[0x60, 0x01, 0x60, 0x02, 0x01, 0x50, 0x00];

const SPEC: SpecId = SpecId::OSAKA;

fn evm() -> Evm<'static, BaseEvmTypes> {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &CALLER,
        AccountInfo { balance: U256::from(10).pow(U256::from(20)), ..Default::default() },
    );
    for (address, code) in [(TARGET, LOOP), (CHILD, CHILD_CODE)] {
        let code = Bytecode::new_legacy(Bytes::from_static(code));
        db.insert_account_info(
            &address,
            AccountInfo { code_hash: code.hash_slow(), code: Some(code), ..Default::default() },
        );
    }
    Evm::new(SPEC, BlockEnvExt::default(), ethereum_tx_registry(SPEC), db, Precompiles::base(SPEC))
}

const fn tx() -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: 16_000_000,
            to: TxKind::Call(TARGET),
            value: U256::ZERO,
            input: Bytes::new(),
        }),
        CALLER,
    )
}

fn run(
    evm: &mut Evm<'static, BaseEvmTypes>,
    tx: &RecoveredTxEnvelope,
    inspector: &mut dyn Inspector<BaseEvmTypes>,
) {
    let result = evm.transact_with_inspector(tx, inspector).unwrap();
    assert!(result.result().is_success());
}

fn inspectors(c: &mut Criterion) {
    let mut group = c.benchmark_group("inspectors");
    let tx = tx();
    let mut evm = evm();

    let configs = [
        ("geth_default", TracingInspectorConfig::default_geth()),
        ("parity_vm_trace", TracingInspectorConfig::parity_vm_trace()),
        ("call_frames", TracingInspectorConfig::from_geth_call_config(&CallConfig::default())),
        ("all", TracingInspectorConfig::all()),
    ];
    for (name, config) in configs {
        let mut inspector = TracingInspector::new(config);
        group.bench_function(name, |b| {
            b.iter(|| {
                inspector.fuse();
                run(&mut evm, &tx, &mut inspector);
                black_box(inspector.traces().nodes().len())
            })
        });
    }

    group.bench_function("opcode_gas", |b| {
        b.iter(|| {
            let mut inspector = OpcodeGasInspector::new();
            run(&mut evm, &tx, &mut inspector);
            black_box(inspector)
        })
    });

    let mut inspector = TracingInspector::new(TracingInspectorConfig::default_geth());
    run(&mut evm, &tx, &mut inspector);
    group.bench_function("geth_struct_logs_builder", |b| {
        b.iter(|| {
            black_box(inspector.geth_builder().geth_traces(
                0,
                Bytes::new(),
                GethDefaultTracingOptions::default(),
            ))
        })
    });

    let mut inspector = TracingInspector::new(TracingInspectorConfig::from_geth_call_config(
        &CallConfig::default(),
    ));
    run(&mut evm, &tx, &mut inspector);
    group.bench_function("geth_call_builder", |b| {
        b.iter(|| black_box(inspector.geth_builder().geth_call_traces(CallConfig::default(), 0)))
    });

    group.finish();
}

criterion_group!(benches, inspectors);
criterion_main!(benches);
