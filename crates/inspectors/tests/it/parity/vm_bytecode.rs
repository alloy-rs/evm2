use crate::utils::{AccountInfo, Bytecode, CacheDB, Context, EmptyDB, SpecId, TransactTo, TxEnv};
use alloy_consensus::{TxEip7702, transaction::Recovered};
use alloy_eips::eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization};
use alloy_primitives::{Address, Bytes, U256, hex, map::HashSet};
use alloy_rpc_types_trace::parity::{TraceResults, TraceType};
use evm2::{
    BaseEvmTypes, Evm, Precompiles,
    ethereum::{LazyTxEip7702, RecoveredTxEnvelope, TxEnvelope, ethereum_tx_registry},
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

fn trace(db: CacheDB<EmptyDB>, tx: TxEnv) -> TraceResults {
    trace_envelope(db, tx.envelope())
}

fn trace_envelope(mut db: CacheDB<EmptyDB>, tx: RecoveredTxEnvelope) -> TraceResults {
    let types = HashSet::from_iter([TraceType::VmTrace]);
    let mut inspector = TracingInspector::new(TracingInspectorConfig::from_parity_config(&types));
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::PRAGUE,
        Default::default(),
        ethereum_tx_registry(SpecId::PRAGUE),
        db.clone(),
        Precompiles::base(SpecId::PRAGUE),
    );
    evm.set_inspector(&mut inspector);
    let result = evm.transact(&tx).unwrap().detach();
    drop(evm);
    let builder = inspector.into_parity_builder();
    let direct = builder.vm_trace();
    let traces = builder.into_trace_results_with_state(&result, &types, &mut db).unwrap();
    assert_eq!(Some(direct), traces.vm_trace);
    traces
}

#[test]
fn constructor_bytecode() {
    let initcode = hex!("60016000526001601ff3");
    let traces = trace(
        CacheDB::default(),
        TxEnv::builder()
            .kind(TransactTo::Create)
            .data(initcode.into())
            .gas_limit(100_000)
            .build_fill(),
    );
    assert_eq!(traces.output.as_ref(), [0x01]);
    let vm = traces.vm_trace.unwrap();
    assert_eq!(vm.code.as_ref(), initcode);
    assert_eq!(vm.ops.last().unwrap().op.as_deref(), Some("RETURN"));
}

#[test]
fn nested_creation_bytecode() {
    for create2 in [false, true] {
        // Store initcode that returns STOP at offset 24, then CREATE or CREATE2 it.
        let initcode = hex!("60005f5360015ff3");
        let mut code = hex!("6760005f5360015ff35f52").to_vec();
        if create2 {
            code.push(0x5f);
        }
        code.extend(hex!("600860185f"));
        code.push(if create2 { 0xf5 } else { 0xf0 });
        code.push(0x00);
        let target = Address::with_last_byte(0x42);
        let mut db = CacheDB::default();
        db.insert_account_info(
            &target,
            AccountInfo::default().with_code(Bytecode::new_raw(code.clone().into())),
        );
        let traces = trace(
            db,
            TxEnv::builder().kind(TransactTo::Call(target)).gas_limit(200_000).build_fill(),
        );
        let vm = traces.vm_trace.unwrap();
        assert_eq!(vm.code.as_ref(), code);
        let child = vm.ops.iter().find_map(|op| op.sub.as_ref()).unwrap();
        assert_eq!(child.code.as_ref(), initcode);
        assert_eq!(child.ops.last().unwrap().op.as_deref(), Some("RETURN"));
    }
}

#[test]
fn authorization_resolves_executed_bytecode() {
    let authority = Address::with_last_byte(0x42);
    let old_delegate = Address::with_last_byte(0x43);
    let delegate = Address::with_last_byte(0x44);
    for before in [Bytecode::default(), Bytecode::new_eip7702(old_delegate)] {
        for (delegate, code) in [
            (delegate, Bytes::from_static(&hex!("602a5f5260205ff3"))),
            (delegate, Bytes::from_static(&hex!("5f5ffd"))),
            (Address::ZERO, Bytes::new()),
        ] {
            let mut db = CacheDB::default();
            db.insert_account_info(&authority, AccountInfo::default().with_code(before.clone()));
            db.insert_account_info(
                &old_delegate,
                AccountInfo::default().with_code(Bytecode::new_raw(hex!("60015000").into())),
            );
            if !delegate.is_zero() {
                db.insert_account_info(
                    &delegate,
                    AccountInfo::default().with_code(Bytecode::new_raw(code.clone())),
                );
            }
            let traces = trace_envelope(
                db,
                Recovered::new_unchecked(
                    TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
                        TxEip7702 {
                            chain_id: 1,
                            to: authority,
                            gas_limit: 100_000,
                            ..Default::default()
                        },
                        vec![RecoveredAuthorization::new_unchecked(
                            Authorization { chain_id: U256::from(1), address: delegate, nonce: 0 },
                            RecoveredAuthority::Valid(authority),
                        )],
                    )),
                    Address::ZERO,
                ),
            );
            let vm = traces.vm_trace.unwrap();
            assert_eq!(vm.code, code);
            if code.is_empty() {
                assert!(vm.ops.is_empty());
            } else {
                assert!(!vm.ops.is_empty());
            }
        }
    }
}

#[test]
fn callcode_and_delegatecall_bytecode() {
    let target = Address::with_last_byte(0x42);
    let child = Address::with_last_byte(0x43);
    let child_code = hex!("602a5f5260205ff3");
    for code in [hex!("5f5f5f5f5f604361fffff200").to_vec(), hex!("5f5f5f5f604361fffff400").to_vec()]
    {
        let mut db = CacheDB::default();
        db.insert_account_info(
            &target,
            AccountInfo::default().with_code(Bytecode::new_raw(code.clone().into())),
        );
        db.insert_account_info(
            &child,
            AccountInfo::default().with_code(Bytecode::new_raw(child_code.into())),
        );
        let traces = trace(
            db,
            TxEnv::builder().kind(TransactTo::Call(target)).gas_limit(100_000).build_fill(),
        );
        let vm = traces.vm_trace.unwrap();
        assert_eq!(vm.code.as_ref(), code);
        let sub = vm.ops.iter().find_map(|op| op.sub.as_ref()).unwrap();
        assert_eq!(sub.code.as_ref(), child_code);
    }
}

#[test]
fn bytecode_recording_is_opt_in() {
    let code = hex!("60015000");
    for config in [
        TracingInspectorConfig::default_geth(),
        TracingInspectorConfig::default_parity(),
        TracingInspectorConfig::parity_statediff(),
    ] {
        let inspector = inspect_code(&code, config);
        assert!(inspector.traces().nodes().iter().all(|node| node.trace.bytecode.is_none()));
    }
    let mut config = TracingInspectorConfig::default_geth();
    config.merge(TracingInspectorConfig::parity_vm_trace());
    let inspector = inspect_code(&code, config);
    assert_eq!(inspector.traces().nodes()[0].trace.bytecode.as_ref().unwrap().as_ref(), code);
}

#[test]
fn bytecode_recording_reuses_original_buffer() {
    // Keep a full contract-sized buffer alive so copying it is observable by pointer identity.
    let mut bytes = vec![0x5b; 24_576];
    bytes[0] = 0x00;
    let code = Bytecode::new_raw(bytes.into());
    let inspector = inspect_bytecode(code.clone(), TracingInspectorConfig::parity_vm_trace());
    let recorded = inspector.traces().nodes()[0].trace.bytecode.as_ref().unwrap();
    assert_eq!(recorded.as_ptr(), code.original_byte_slice().as_ptr());
    assert_eq!(recorded.len(), code.len());

    let vm_trace = inspector.into_parity_builder().vm_trace();
    assert_eq!(vm_trace.code.as_ptr(), code.original_byte_slice().as_ptr());
    assert_eq!(vm_trace.code.len(), code.len());
}

fn inspect_code(code: &[u8], config: TracingInspectorConfig) -> TracingInspector {
    inspect_bytecode(Bytecode::new_raw(Bytes::copy_from_slice(code)), config)
}

fn inspect_bytecode(code: Bytecode, config: TracingInspectorConfig) -> TracingInspector {
    let target = Address::with_last_byte(0x42);
    let mut db = CacheDB::default();
    db.insert_account_info(&target, AccountInfo::default().with_code(code));
    let mut evm =
        Context::mainnet().with_db(db).build_mainnet_with_inspector(TracingInspector::new(config));
    let result = evm
        .inspect_tx(TxEnv::builder().kind(TransactTo::Call(target)).gas_limit(100_000).build_fill())
        .unwrap();
    assert!(result.result.is_success());
    evm.into_inspector()
}
