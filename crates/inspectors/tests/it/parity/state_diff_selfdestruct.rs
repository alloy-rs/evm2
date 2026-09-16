use crate::utils::{AccountInfo, Bytecode, CacheDB, Context, EmptyDB, SpecId, TransactTo, TxEnv};
use alloy_primitives::{Address, B256, U64, U256, hex, map::HashSet};
use alloy_rpc_types_trace::parity::{Delta, TraceType};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

#[test]
fn existing_account_selfdestruct_across_cancun() {
    for spec in [SpecId::SHANGHAI, SpecId::CANCUN] {
        for revert in [false, true] {
            let target = Address::with_last_byte(0x42);
            let parent = Address::with_last_byte(0x43);
            // Read an unchanged slot, overwrite a nonzero slot, write a new slot, read zero,
            // then selfdestruct. Deletion must use the original values, including read-only slots.
            let code = Bytecode::new_raw(
                hex!("600054506009600155600360025560035450600060045533ff").into(),
            );
            let mut db = CacheDB::<EmptyDB>::default();
            db.insert_account_info(
                &target,
                AccountInfo { balance: U256::from(69), nonce: 1, ..Default::default() }
                    .with_code(code.clone()),
            );
            db.insert_account_storage(&target, &U256::ZERO, &U256::from(7));
            db.insert_account_storage(&target, &U256::from(1), &U256::from(8));
            db.insert_account_storage(&target, &U256::from(4), &U256::from(10));
            db.insert_account_info(
                &parent,
                AccountInfo::default().with_code(Bytecode::new_raw(
                    hex!("60006000600060006000604261fffff15060006000fd").into(),
                )),
            );
            let mut evm = Context::mainnet()
                .modify_cfg_chained(|cfg| cfg.spec = spec)
                .with_db(db)
                .build_mainnet_with_inspector(TracingInspector::new(
                    TracingInspectorConfig::default_parity(),
                ));
            let result = evm
                .inspect_tx(
                    TxEnv::builder()
                        .kind(TransactTo::Call(if revert { parent } else { target }))
                        .gas_limit(200_000)
                        .build_fill(),
                )
                .unwrap();
            assert_eq!(result.result.is_success(), !revert);
            assert_eq!(
                result.state.account_info(&target).is_none(),
                spec < SpecId::CANCUN && !revert
            );
            let traces = evm
                .inspector
                .into_parity_builder()
                .into_trace_results_with_state(
                    &result.tx_result,
                    &HashSet::from_iter([TraceType::StateDiff]),
                    &mut evm.ctx.db,
                )
                .unwrap();
            let state_diff = traces.state_diff.unwrap();
            if revert {
                assert!(!state_diff.contains_key(&target));
                continue;
            }
            let diff = &state_diff[&target];
            if spec < SpecId::CANCUN {
                assert_eq!(diff.balance, Delta::Removed(U256::from(69)));
                assert_eq!(diff.nonce, Delta::Removed(U64::from(1)));
                assert_eq!(diff.code, Delta::Removed(code.original_bytes()));
                assert_eq!(diff.storage.len(), 3);
                assert_eq!(diff.storage[&B256::ZERO], Delta::Removed(U256::from(7).into()));
                assert_eq!(
                    diff.storage[&B256::with_last_byte(1)],
                    Delta::Removed(U256::from(8).into())
                );
                assert_eq!(
                    diff.storage[&B256::with_last_byte(4)],
                    Delta::Removed(U256::from(10).into())
                );
                let json = serde_json::to_value(diff).unwrap();
                assert_eq!(json["nonce"], serde_json::json!({"-": "0x1"}));
            } else {
                assert_eq!(diff.balance, Delta::changed(U256::from(69), U256::ZERO));
                assert_eq!(diff.nonce, Delta::Unchanged);
                assert_eq!(diff.code, Delta::Unchanged);
                assert_eq!(
                    diff.storage[&B256::with_last_byte(4)],
                    Delta::changed(U256::from(10).into(), U256::ZERO.into())
                );
                assert_eq!(diff.storage.len(), 3);
                assert_eq!(
                    diff.storage[&B256::with_last_byte(1)],
                    Delta::changed(U256::from(8).into(), U256::from(9).into())
                );
                assert_eq!(
                    diff.storage[&B256::with_last_byte(2)],
                    Delta::changed(U256::ZERO.into(), U256::from(3).into())
                );
            }
        }
    }
}

#[test]
fn created_account_selfdestruct_across_cancun() {
    for spec in [SpecId::SHANGHAI, SpecId::CANCUN] {
        for prefunded in [false, true] {
            let caller = Address::with_last_byte(0x42);
            let target = caller.create(0);
            let mut db = CacheDB::<EmptyDB>::default();
            if prefunded {
                db.insert_account_info(
                    &target,
                    AccountInfo { balance: U256::from(69), ..Default::default() },
                );
            }
            let mut evm = Context::mainnet()
                .modify_cfg_chained(|cfg| cfg.spec = spec)
                .with_db(db)
                .build_mainnet_with_inspector(TracingInspector::new(
                    TracingInspectorConfig::default_parity(),
                ));
            let result = evm
                .inspect_tx(
                    TxEnv::builder()
                        .caller(caller)
                        .kind(TransactTo::Create)
                        .data(hex!("33ff").into())
                        .gas_limit(100_000)
                        .build_fill(),
                )
                .unwrap();
            assert!(result.result.is_success());
            assert!(result.state.account_info(&target).is_none());
            let traces = evm
                .inspector
                .into_parity_builder()
                .into_trace_results_with_state(
                    &result.tx_result,
                    &HashSet::from_iter([TraceType::StateDiff]),
                    &mut evm.ctx.db,
                )
                .unwrap();
            let diff = traces.state_diff.unwrap();
            if prefunded {
                let diff = &diff[&target];
                assert_eq!(diff.balance, Delta::Removed(U256::from(69)));
                assert_eq!(diff.nonce, Delta::Removed(U64::ZERO));
                assert_eq!(diff.code, Delta::Removed(Default::default()));
                assert!(diff.storage.is_empty());
            } else {
                assert!(!diff.contains_key(&target));
            }
        }
    }
}

#[test]
fn selfdestruct_preserves_balance_only_account() {
    let caller = Address::with_last_byte(0x42);
    let target = caller.create(0);
    let mut db = CacheDB::default();
    db.insert_account_info(&target, AccountInfo { balance: U256::from(69), ..Default::default() });
    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| cfg.spec = SpecId::AMSTERDAM)
        .with_db(db)
        .build_mainnet_with_inspector(TracingInspector::new(
            TracingInspectorConfig::default_parity(),
        ));
    // EIP-8246 preserves the balance when a newly created account selfdestructs to itself.
    let result = evm
        .inspect_tx(
            TxEnv::builder()
                .caller(caller)
                .kind(TransactTo::Create)
                .data(hex!("30ff").into())
                .gas_limit(200_000)
                .build_fill(),
        )
        .unwrap();
    assert!(result.result.is_success());
    let account = result.state.account_info(&target).unwrap();
    assert_eq!(account.balance, U256::from(69));
    assert_eq!(account.nonce, 0);
    let traces = evm
        .inspector
        .into_parity_builder()
        .into_trace_results_with_state(
            &result.tx_result,
            &HashSet::from_iter([TraceType::StateDiff]),
            &mut evm.ctx.db,
        )
        .unwrap();
    if let Some(diff) = traces.state_diff.unwrap().get(&target) {
        assert_eq!(diff.balance, Delta::Unchanged);
        assert_eq!(diff.nonce, Delta::Unchanged);
        assert!(!diff.code.is_removed());
    }
}
