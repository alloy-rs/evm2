use alloy_consensus::{TxEip7702, transaction::Recovered};
use alloy_eips::eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization};
use alloy_primitives::{Address, U64, U256, hex, map::HashSet};
use alloy_rpc_types_trace::parity::{Delta, TraceType};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    ethereum::{LazyTxEip7702, TxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, CacheDB, EmptyDB},
};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

#[test]
fn authorization_code_changes() {
    let old_delegate = Address::with_last_byte(0x41);
    let new_delegate = Address::with_last_byte(0x42);
    for (before, delegate) in [
        (Bytecode::default(), new_delegate),
        (Bytecode::new_eip7702(old_delegate), new_delegate),
        (Bytecode::new_eip7702(old_delegate), Address::ZERO),
        (Bytecode::new_eip7702(new_delegate), new_delegate),
    ] {
        // Authorization changes persist even when transaction execution reverts.
        for revert in [false, true] {
            let authority = Address::with_last_byte(0x43);
            let target = Address::with_last_byte(0x44);
            let mut db = CacheDB::<EmptyDB>::default();
            db.insert_account_info(
                &authority,
                AccountInfo { nonce: 1, ..Default::default() }.with_code(before.clone()),
            );
            db.insert_account_info(
                &target,
                AccountInfo::default().with_code(Bytecode::new_raw(
                    if revert { hex!("5f5ffd").to_vec() } else { vec![0x00] }.into(),
                )),
            );
            let mut inspector = TracingInspector::new(TracingInspectorConfig::default_parity());
            let mut evm = Evm::<BaseEvmTypes>::new(
                SpecId::PRAGUE,
                Default::default(),
                ethereum_tx_registry(SpecId::PRAGUE),
                db.clone(),
                Precompiles::base(SpecId::PRAGUE),
            );
            evm.set_inspector(&mut inspector);
            let tx = Recovered::new_unchecked(
                TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
                    TxEip7702 { chain_id: 1, to: target, gas_limit: 100_000, ..Default::default() },
                    vec![RecoveredAuthorization::new_unchecked(
                        Authorization { chain_id: U256::from(1), address: delegate, nonce: 1 },
                        RecoveredAuthority::Valid(authority),
                    )],
                )),
                Address::ZERO,
            );
            let result = evm.transact(&tx).unwrap().detach();
            drop(evm);
            assert_eq!(result.result.status, !revert);
            let after = if delegate.is_zero() {
                Bytecode::default()
            } else {
                Bytecode::new_eip7702(delegate)
            };
            assert_eq!(
                result.pending_state.account_info(&authority).unwrap().code_hash,
                after.hash_slow()
            );
            let traces = inspector
                .into_parity_builder()
                .into_trace_results_with_state(
                    &result,
                    &HashSet::from_iter([TraceType::StateDiff]),
                    &mut db,
                )
                .unwrap();
            let diff = &traces.state_diff.unwrap()[&authority];
            let expected = if before == after {
                Delta::Unchanged
            } else {
                Delta::changed(before.original_bytes(), after.original_bytes())
            };
            assert_eq!(diff.code, expected, "delegate={delegate}, revert={revert}");
            assert_eq!(diff.nonce, Delta::changed(U64::from(1), U64::from(2)));
            assert_eq!(diff.balance, Delta::Unchanged);
        }
    }
}
