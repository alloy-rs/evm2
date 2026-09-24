use crate::utils::{
    AccountInfo, CacheDB, Context, DatabaseCommit, EmptyDB, SpecId, TransactTo, TxEnv,
};
use alloy_primitives::{Address, U64, U256, map::HashSet};
use alloy_rpc_types_trace::parity::{Delta, StateDiff, TraceType};
use evm2_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

fn trace(db: &mut CacheDB<EmptyDB>, tx: TxEnv, beneficiary: Address, spec: SpecId) -> StateDiff {
    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| cfg.spec = spec)
        .modify_block_chained(|block| block.beneficiary = beneficiary)
        .with_db(db.clone())
        .build_mainnet_with_inspector(TracingInspector::new(
            TracingInspectorConfig::default_parity(),
        ));
    let result = evm.inspect_tx(tx).unwrap();
    assert!(result.result.is_success());
    let traces = evm
        .inspector
        .into_parity_builder()
        .into_trace_results_with_state(
            &result.tx_result,
            &HashSet::from_iter([TraceType::StateDiff]),
            &mut evm.ctx.db,
        )
        .unwrap();
    db.commit(result.state);
    traces.state_diff.unwrap()
}

#[test]
fn transfer_creates_account_then_changes_existing_account() {
    let caller = Address::with_last_byte(0x41);
    let recipient = Address::with_last_byte(0x42);
    let beneficiary = Address::with_last_byte(0x43);
    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(1_000_000), ..Default::default() },
    );
    for nonce in 0..2 {
        let diff = trace(
            &mut db,
            TxEnv::builder()
                .caller(caller)
                .kind(TransactTo::Call(recipient))
                .value(U256::from(7))
                .nonce(nonce)
                .gas_price(1)
                .gas_limit(21_000)
                .build_fill(),
            beneficiary,
            SpecId::PRAGUE,
        );
        for (address, amount) in [(recipient, 7), (beneficiary, 21_000)] {
            let account = &diff[&address];
            if nonce == 0 {
                assert_eq!(account.balance, Delta::Added(U256::from(amount)));
                assert_eq!(account.nonce, Delta::Added(U64::ZERO));
                assert_eq!(account.code, Delta::Added(Default::default()));
            } else {
                assert_eq!(
                    account.balance,
                    Delta::changed(U256::from(amount), U256::from(2 * amount))
                );
                assert_eq!(account.nonce, Delta::Unchanged);
                assert_eq!(account.code, Delta::Unchanged);
            }
            assert!(account.storage.is_empty());
        }
    }
}

#[test]
fn zero_value_call_does_not_create_account() {
    let mut db = CacheDB::<EmptyDB>::default();
    let recipient = Address::with_last_byte(0x42);
    let beneficiary = Address::with_last_byte(0x43);
    let diff = trace(
        &mut db,
        TxEnv::builder().kind(TransactTo::Call(recipient)).build_fill(),
        beneficiary,
        SpecId::PRAGUE,
    );
    assert!(!diff.contains_key(&recipient));
    assert!(!diff.contains_key(&beneficiary));
}

#[test]
fn creation_uses_account_existence_not_initial_balance() {
    let caller = Address::with_last_byte(0x41);
    let target = caller.create(0);
    for initial_balance in [None, Some(0), Some(9)] {
        for runtime in [false, true] {
            let mut db = CacheDB::<EmptyDB>::default();
            if let Some(balance) = initial_balance {
                db.insert_account_info(
                    &target,
                    AccountInfo { balance: U256::from(balance), ..Default::default() },
                );
            }
            // Return either empty runtime or one STOP byte.
            let initcode = vec![0x60, u8::from(runtime), 0x60, 0, 0xf3];
            let diff = trace(
                &mut db,
                TxEnv::builder()
                    .caller(caller)
                    .kind(TransactTo::Create)
                    .data(initcode.into())
                    .gas_limit(100_000)
                    .build_fill(),
                Address::ZERO,
                SpecId::PRAGUE,
            );
            let account = &diff[&target];
            let code = if runtime { vec![0].into() } else { Default::default() };
            if initial_balance.is_none() {
                assert_eq!(account.balance, Delta::Added(U256::ZERO));
                assert_eq!(account.nonce, Delta::Added(U64::from(1)));
                assert_eq!(account.code, Delta::Added(code));
            } else {
                assert_eq!(account.balance, Delta::Unchanged);
                assert_eq!(account.nonce, Delta::changed(U64::ZERO, U64::from(1)));
                assert_eq!(
                    account.code,
                    if runtime {
                        Delta::changed(Default::default(), code)
                    } else {
                        Delta::Unchanged
                    }
                );
            }
            assert!(account.storage.is_empty());
        }
    }
}

#[test]
fn frontier_materializes_empty_account() {
    let caller = Address::with_last_byte(0x41);
    let recipient = Address::with_last_byte(0x42);
    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(100_000), ..Default::default() },
    );
    let diff = trace(
        &mut db,
        TxEnv::builder()
            .caller(caller)
            .kind(TransactTo::Call(recipient))
            .gas_limit(21_000)
            .build_fill(),
        Address::ZERO,
        SpecId::FRONTIER,
    );
    let account = &diff[&recipient];
    assert_eq!(account.balance, Delta::Added(U256::ZERO));
    assert_eq!(account.nonce, Delta::Added(U64::ZERO));
    assert_eq!(account.code, Delta::Added(Default::default()));
}
