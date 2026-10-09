//! TIP1143-T41/T42: real authorization hook dispatch and default gas guards.
//! Cached recovered signatures isolate authorization execution from cryptography.

use alloy_consensus::{TxEip7702, transaction::Recovered};
use alloy_eips::eip7702::{
    Authorization, RecoveredAuthority, RecoveredAuthorization, SignedAuthorization,
};
use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::{
    BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, SpecId, TxResult, Version,
    bytecode::{Bytecode, CodeChunk},
    env::BlockEnvExt,
    ethereum::{
        LazyTxEip7702, TxEnvelope,
        eip7702::{self, AppliedAuth, AuthAccounting, AuthRefunds, AuthorizationResult},
    },
    evm::{AccountInfo, Database, Db, InMemoryDB},
    handler::{DefaultTxHandlerHooks, GasSettlement, TxHandlerHooks},
    interpreter::{GasTracker, InstrStop, Word},
    registry::{HandlerError, HandlerResult, TxRegistry, handler},
};
use std::{
    cell::{Cell, RefCell},
    io,
    rc::Rc,
};

thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct Delegating;
struct AuthorizationOog;
struct AuthorizationError;

fn observe(envelope: &TxEnvelope, tx: &LazyTxEip7702, caller: Address) {
    CALLS.with(|calls| calls.set(calls.get() + 1));
    assert_eq!(caller, Address::repeat_byte(0xbb));
    assert!(std::ptr::eq(envelope.as_eip7702().unwrap(), tx));
    assert_eq!(tx.to, Address::repeat_byte(0xaa));
    assert!(!tx.authorization_list.is_empty());
}

impl TxHandlerHooks<BaseEvmTypes> for Delegating {
    fn apply_authorizations(
        host: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        gas: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        observe(envelope, tx, caller);
        eip7702::apply_authorizations(host, tx, caller, gas)
    }
}

impl TxHandlerHooks<BaseEvmTypes> for AuthorizationOog {
    fn apply_authorizations(
        host: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        gas: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        observe(envelope, tx, caller);
        let applied = eip7702::apply_authorizations(host, tx, caller, gas)?;
        assert!(!applied.out_of_gas);
        let authority = host.state_mut().account(&Address::repeat_byte(0xcc))?;
        assert_eq!(authority.get().unwrap().nonce, 1);
        Ok(AuthorizationResult { out_of_gas: true, state_refund: 0, execution_refund: 0 })
    }
}

impl TxHandlerHooks<BaseEvmTypes> for AuthorizationError {
    fn apply_authorizations(
        _: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        _: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        observe(envelope, tx, caller);
        Err(HandlerError::external(io::Error::other("authorization sentinel")))
    }
}

fn fixture<H: TxHandlerHooks<BaseEvmTypes> + 'static>(
    enabled: bool,
    spec: SpecId,
    state_gas: bool,
    gas_limit: u64,
) -> (Evm<'static, BaseEvmTypes>, Recovered<TxEnvelope>) {
    fixture_details::<H>(
        enabled,
        spec,
        state_gas,
        gas_limit,
        None,
        vec![(0, Address::repeat_byte(0xdd))],
        Bytes::new(),
    )
}

fn fixture_details<H: TxHandlerHooks<BaseEvmTypes> + 'static>(
    enabled: bool,
    spec: SpecId,
    state_gas: bool,
    gas_limit: u64,
    authority: Option<AccountInfo>,
    entries: Vec<(u64, Address)>,
    recipient_code: Bytes,
) -> (Evm<'static, BaseEvmTypes>, Recovered<TxEnvelope>) {
    CALLS.with(|calls| calls.set(0));
    let caller = Address::repeat_byte(0xbb);
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &caller,
        AccountInfo { balance: U256::from(u64::MAX), ..Default::default() },
    );
    if let Some(info) = authority {
        db.insert_account_info(&Address::repeat_byte(0xcc), info);
    }
    if !recipient_code.is_empty() {
        let code = Bytecode::new_legacy(recipient_code);
        db.insert_account_info(
            &Address::repeat_byte(0xaa),
            AccountInfo { code_hash: code.hash_slow(), code: Some(code), ..Default::default() },
        );
    }
    let mut version = Version::new(spec).with_tip1143(enabled);
    version.features.set(EvmFeatures::EIP8037, state_gas);
    let chain_id = version.chain_id;
    let mut registry = TxRegistry::new();
    registry.register(
        4,
        TxEnvelope::as_eip7702,
        handler(
            eip7702::prepare_with_hooks::<BaseEvmTypes, H>,
            eip7702::execute_prepared::<BaseEvmTypes, H>,
        ),
    );
    let evm = Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(spec, version),
        spec,
        BlockEnvExt::default(),
        registry,
        db,
        Precompiles::base(spec),
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
            TxEip7702 { chain_id, to: Address::repeat_byte(0xaa), gas_limit, ..Default::default() },
            entries
                .into_iter()
                .map(|(nonce, address)| {
                    RecoveredAuthorization::new_unchecked(
                        Authorization { chain_id: U256::ZERO, address, nonce },
                        RecoveredAuthority::Valid(Address::repeat_byte(0xcc)),
                    )
                })
                .collect(),
        )),
        caller,
    );
    (evm, tx)
}

#[test]
fn tip1143_t41_delegating_hook_dispatches_once_in_both_modes() {
    for enabled in [false, true] {
        let (mut evm, tx) = fixture::<Delegating>(enabled, SpecId::PRAGUE, false, 100_000);
        let result = evm.transact(&tx).unwrap().detach();
        CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert!(result.result.status);
        assert_eq!(result.result.total_gas_spent, 46_000);
        assert_eq!(result.result.state_gas_spent, 0);
        assert!(result.result.logs.is_empty());
        let info = result.pending_state.account_info(&Address::repeat_byte(0xcc)).unwrap();
        assert_eq!(info.nonce, 1);
        assert_eq!(info.code_hash, Bytecode::new_eip7702(Address::repeat_byte(0xdd)).hash_slow());
        assert!(info.code_metadata.is_none());
    }
}

#[test]
fn tip1143_t41_hook_oog_rolls_back_applied_authorization() {
    for enabled in [false, true] {
        let (mut evm, tx) = fixture::<AuthorizationOog>(enabled, SpecId::PRAGUE, false, 100_000);
        let result = evm.transact(&tx).unwrap().detach();
        CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert!(!result.result.status);
        assert_eq!(result.result.total_gas_spent, 100_000);
        assert!(result.pending_state.account_info(&Address::repeat_byte(0xcc)).is_none());
        assert!(result.result.logs.is_empty());
    }
}

#[test]
fn tip1143_t41_hook_error_remains_node_error() {
    for enabled in [false, true] {
        let (mut evm, tx) = fixture::<AuthorizationError>(enabled, SpecId::PRAGUE, false, 100_000);
        assert!(evm.transact(&tx).is_err());
        CALLS.with(|calls| assert_eq!(calls.get(), 1));
    }
}

#[test]
fn tip1143_t42_eip2780_without_eip8037_exact_budget() {
    // c5e4c6c golden: TX_BASE + cold recipient + authorization base + ACCOUNT_WRITE.
    let exact = 12_000 + 3_000 + 7_816 + 9_000;
    for enabled in [false, true] {
        for budget in [exact - 1, exact, exact + 1] {
            let (mut evm, tx) = fixture::<Delegating>(enabled, SpecId::AMSTERDAM, false, budget);
            let result = evm.transact(&tx).unwrap().detach();
            CALLS.with(|calls| assert_eq!(calls.get(), 1));
            assert_eq!(result.result.status, budget >= exact);
            assert_eq!(result.result.state_gas_spent, 0);
            assert_eq!(result.result.total_gas_spent, budget.min(exact));
            let info = result.pending_state.account_info(&Address::repeat_byte(0xcc));
            if budget >= exact {
                assert_eq!(info.unwrap().nonce, 1);
            } else {
                assert!(info.is_none());
            }
        }
    }
}

#[test]
fn tip1143_t42_default_hook_fixed_goldens() {
    for enabled in [false, true] {
        for (spec, budget, expected_spent) in
            [(SpecId::PRAGUE, 100_000, 46_000), (SpecId::AMSTERDAM, 100_000, 31_816)]
        {
            let (mut evm, tx) = fixture::<DefaultTxHandlerHooks>(enabled, spec, false, budget);
            let result = evm.transact(&tx).unwrap().detach();
            assert!(result.result.status);
            assert_eq!(result.result.total_gas_spent, expected_spent);
            assert_eq!(result.result.state_gas_spent, 0);
            assert_eq!(result.result.refunded, 0);
            assert!(result.result.logs.is_empty());
            let info = result.pending_state.account_info(&Address::repeat_byte(0xcc)).unwrap();
            assert_eq!(info.nonce, 1);
            assert_eq!(
                info.code_hash,
                Bytecode::new_eip7702(Address::repeat_byte(0xdd)).hash_slow()
            );
        }
    }
}

struct IntrinsicAuth;

impl AuthAccounting for IntrinsicAuth {
    fn rejected(&mut self) {}

    fn accepted(&mut self, _: Address, _: &AppliedAuth) -> Result<(), InstrStop> {
        Ok(())
    }
}

struct TempoPolicy;

impl TxHandlerHooks<BaseEvmTypes> for TempoPolicy {
    fn apply_authorizations(
        host: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        gas: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        observe(envelope, tx, caller);
        if host.feature(EvmFeatures::EIP8037) {
            let chain_id = host.version().chain_id;
            let out_of_gas = eip7702::apply_auth_list(
                host,
                chain_id,
                &tx.authorization_list,
                &mut IntrinsicAuth,
            )?;
            Ok(AuthorizationResult { out_of_gas, ..Default::default() })
        } else {
            eip7702::apply_authorizations(host, tx, caller, gas)
        }
    }
}

#[test]
fn tip1143_t41_tempo_intrinsic_policy_controls_runtime_authorization_charges() {
    for enabled in [false, true] {
        for state_gas in [false, true] {
            let (mut evm, tx) =
                fixture::<TempoPolicy>(enabled, SpecId::AMSTERDAM, state_gas, 200_000);
            let result = evm.transact(&tx).unwrap().detach();
            CALLS.with(|calls| assert_eq!(calls.get(), 1));
            assert!(result.result.status);
            // This isolates the downstream hook's accounting choice. Tempo's own
            // intrinsic-price configuration is a separate downstream responsibility.
            assert_eq!(result.result.total_gas_spent, if state_gas { 22_816 } else { 31_816 });
            assert_eq!(result.result.state_gas_spent, 0);
            assert_eq!(result.result.refunded, 0);
            let info = result.pending_state.account_info(&Address::repeat_byte(0xcc)).unwrap();
            assert_eq!(info.nonce, 1);
            assert_eq!(
                info.code_hash,
                Bytecode::new_eip7702(Address::repeat_byte(0xdd)).hash_slow()
            );
        }
    }
}

struct DistinctRefunds;

impl TxHandlerHooks<BaseEvmTypes> for DistinctRefunds {
    fn apply_authorizations(
        host: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        _: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        observe(envelope, tx, caller);
        let chain_id = host.version().chain_id;
        assert!(!eip7702::apply_auth_list(
            host,
            chain_id,
            &tx.authorization_list,
            &mut IntrinsicAuth
        )?);
        Ok(AuthorizationResult { out_of_gas: false, state_refund: 101, execution_refund: 303 })
    }

    fn settle_transaction(
        host: &mut Evm<'_, BaseEvmTypes>,
        _: &TxEnvelope,
        gas: GasSettlement<BaseEvmTypes>,
    ) -> HandlerResult<TxResult<BaseEvmTypes>> {
        // Inspect settlement inputs before refund caps/saturation could conceal
        // ignored or duplicated hook outputs.
        assert_eq!(gas.state_refund, 101);
        assert_eq!(gas.result.gas.refunded(), 303);
        evm2::ethereum::default_settle_gas(host, gas)
    }
}

#[test]
fn tip1143_t41_distinct_hook_refunds_reach_settlement_exactly_once() {
    for enabled in [false, true] {
        let (mut evm, tx) = fixture::<DistinctRefunds>(enabled, SpecId::PRAGUE, false, 100_000);
        let result = evm.transact(&tx).unwrap().detach();
        CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert!(result.result.status);
        assert_eq!(result.result.refunded, 303);
        assert_eq!(result.result.state_gas_spent, 0);
        assert_eq!(
            result.pending_state.account_info(&Address::repeat_byte(0xcc)).unwrap().nonce,
            1
        );
    }
}

#[test]
fn tip1143_t42_refund_accounting_guards_all_authority_facts() {
    // Pinned Prague prices independently exercise the EIP8037 guard on
    // rejected entries and the unconditional existing-authority refund.
    for state_gas in [false, true] {
        let mut version = Version::new(SpecId::PRAGUE);
        version.features.set(EvmFeatures::EIP8037, state_gas);
        for existed in [false, true] {
            for delegated_before_tx in [false, true] {
                for delegated_now in [false, true] {
                    for clearing in [false, true] {
                        let facts =
                            AppliedAuth { existed, delegated_before_tx, delegated_now, clearing };
                        let mut accounting = AuthRefunds::new(&version);
                        accounting.accepted(Address::repeat_byte(0xcc), &facts).unwrap();
                        assert_eq!(accounting.execution_refund, if existed { 12500 } else { 0 });
                        // Prague has zero state prices. Neither enabling the flag
                        // nor changing authority facts may invent a state refund.
                        assert_eq!(accounting.state_refund, 0);
                    }
                }
            }
        }
        let mut rejected = AuthRefunds::new(&version);
        rejected.rejected();
        assert_eq!(rejected.state_refund, 0);
        assert_eq!(rejected.execution_refund, if state_gas { 12500 } else { 0 });
    }
}

#[test]
fn tip1143_t41_t42_existing_rejected_repeated_cleared_and_redelegated_authorities() {
    let delegate = Address::repeat_byte(0xdd);
    let other = Address::repeat_byte(0xee);
    for enabled in [false, true] {
        for initially_delegated in [false, true] {
            for (entries, final_nonce, final_target, refund_count) in [
                (vec![(0, delegate)], 1, delegate, 1u64),
                (
                    vec![(1, delegate)],
                    0,
                    if initially_delegated { other } else { Address::ZERO },
                    0,
                ),
                (vec![(0, delegate), (0, other)], 1, delegate, 1),
                (vec![(0, delegate), (1, other)], 2, other, 2),
                (vec![(0, delegate), (1, Address::ZERO)], 2, Address::ZERO, 2),
                (vec![(0, Address::ZERO), (1, other)], 2, other, 2),
            ] {
                let code = if initially_delegated {
                    Bytecode::new_eip7702(other)
                } else {
                    Bytecode::default()
                };
                let info = AccountInfo {
                    balance: U256::from(1),
                    code_hash: code.hash_slow(),
                    code: Some(code),
                    ..Default::default()
                };
                let count = entries.len() as u64;
                let (mut evm, tx) = fixture_details::<Delegating>(
                    enabled,
                    SpecId::PRAGUE,
                    false,
                    200_000,
                    Some(info.clone()),
                    entries,
                    Bytes::new(),
                );
                let result = evm.transact(&tx).unwrap().detach();
                CALLS.with(|calls| assert_eq!(calls.get(), 1));
                assert!(result.result.status);
                let spent = 21000 + 25000 * count;
                assert_eq!(result.result.total_gas_spent, spent);
                assert_eq!(result.result.refunded, (12500 * refund_count).min(spent / 5));
                assert_eq!(result.result.state_gas_spent, 0);
                // Rejected entries need not emit an account transition.
                let info =
                    result.pending_state.account_info(&Address::repeat_byte(0xcc)).unwrap_or(&info);
                assert_eq!(info.nonce, final_nonce);
                let expected = if final_target.is_zero() {
                    Bytecode::default()
                } else {
                    Bytecode::new_eip7702(final_target)
                };
                assert_eq!(info.code_hash, expected.hash_slow());
                assert!(info.code_metadata.is_none());
            }
        }
    }
}

#[test]
fn tip1143_t41_successful_authorization_survives_recipient_revert() {
    for enabled in [false, true] {
        let (mut evm, tx) = fixture_details::<Delegating>(
            enabled,
            SpecId::PRAGUE,
            false,
            200_000,
            None,
            vec![(0, Address::repeat_byte(0xdd))],
            Bytes::from_static(&[0x60, 0, 0x60, 0, 0xfd]),
        );
        let result = evm.transact(&tx).unwrap().detach();
        CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert!(!result.result.status);
        assert_eq!(result.result.total_gas_spent, 46000 + 6 + if enabled { 28680 } else { 0 });
        assert!(result.result.logs.is_empty());
        let info = result.pending_state.account_info(&Address::repeat_byte(0xcc)).unwrap();
        assert_eq!(info.nonce, 1);
        assert_eq!(info.code_hash, Bytecode::new_eip7702(Address::repeat_byte(0xdd)).hash_slow());
    }
}

struct AdjustedIntrinsic;

impl TxHandlerHooks<BaseEvmTypes> for AdjustedIntrinsic {
    fn adjust_intrinsic_gas(
        _: &mut Evm<'_, BaseEvmTypes>,
        _: &TxEnvelope,
        intrinsic: &mut u64,
        initial_state_gas: &mut u64,
        floor_gas: &mut u64,
    ) -> HandlerResult<()> {
        *intrinsic = 50_000;
        *initial_state_gas = 7_000;
        *floor_gas = 0;
        Ok(())
    }

    fn apply_authorizations(
        host: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        gas: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        Delegating::apply_authorizations(host, envelope, tx, caller, gas)
    }
}

#[test]
fn tip1143_t42_adjusted_intrinsic_plus_state_is_validated_before_authorizations() {
    for enabled in [false, true] {
        for budget in [56_999, 57_000, 57_001] {
            let (mut evm, tx) = fixture::<AdjustedIntrinsic>(enabled, SpecId::PRAGUE, true, budget);
            let validation = evm.validate_tx(&tx);
            let result = evm.transact(&tx).map(|result| result.discard());
            if budget < 57_000 {
                let error = HandlerError::IntrinsicGasTooLow { required: 57_000, got: budget };
                assert_eq!(validation, Err(error.clone()));
                assert_eq!(result.unwrap_err(), error);
                CALLS.with(|calls| assert_eq!(calls.get(), 0));
            } else {
                assert!(validation.is_ok());
                let result = result.unwrap();
                assert!(result.status);
                assert_eq!(result.total_gas_spent, 57_000);
                assert_eq!(result.state_gas_spent, 7_000);
                CALLS.with(|calls| assert_eq!(calls.get(), 1));
            }
        }
    }
}

struct AdjustedFloor;

impl TxHandlerHooks<BaseEvmTypes> for AdjustedFloor {
    fn adjust_intrinsic_gas(
        _: &mut Evm<'_, BaseEvmTypes>,
        _: &TxEnvelope,
        _: &mut u64,
        _: &mut u64,
        floor_gas: &mut u64,
    ) -> HandlerResult<()> {
        *floor_gas = 60_000;
        Ok(())
    }

    fn apply_authorizations(
        host: &mut Evm<'_, BaseEvmTypes>,
        envelope: &TxEnvelope,
        tx: &LazyTxEip7702,
        caller: Address,
        gas: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        Delegating::apply_authorizations(host, envelope, tx, caller, gas)
    }
}

#[test]
fn tip1143_t42_adjusted_floor_survives_authorization_hook_dispatch() {
    for enabled in [false, true] {
        for budget in [59_999, 60_000, 60_001] {
            let (mut evm, tx) = fixture::<AdjustedFloor>(enabled, SpecId::PRAGUE, false, budget);
            let result = evm.transact(&tx).map(|result| result.discard());
            if budget < 60_000 {
                assert_eq!(
                    result.unwrap_err(),
                    HandlerError::IntrinsicGasTooLow { required: 60_000, got: budget }
                );
                CALLS.with(|calls| assert_eq!(calls.get(), 0));
            } else {
                let result = result.unwrap();
                assert!(result.status);
                assert_eq!(result.total_gas_spent, 46_000);
                assert_eq!(result.floor_gas, 60_000);
                assert_eq!(result.tx_gas_used(), 60_000);
                CALLS.with(|calls| assert_eq!(calls.get(), 1));
            }
        }
    }
}

#[test]
fn tip1143_t42_amsterdam_state_refunds_do_not_leak_into_execution_refunds() {
    for enabled in [false, true] {
        for state_gas in [false, true] {
            let mut version = Version::new(SpecId::AMSTERDAM).with_tip1143(enabled);
            version.features.set(EvmFeatures::EIP8037, state_gas);
            for existed in [false, true] {
                for delegated_before_tx in [false, true] {
                    for delegated_now in [false, true] {
                        for clearing in [false, true] {
                            let mut accounting = AuthRefunds::new(&version);
                            accounting
                                .accepted(
                                    Address::repeat_byte(0xcc),
                                    &AppliedAuth {
                                        existed,
                                        delegated_before_tx,
                                        delegated_now,
                                        clearing,
                                    },
                                )
                                .unwrap();
                            let account = if existed { 120 * 1530 } else { 0 };
                            let code = if clearing {
                                23 * 1530 * (1 + u64::from(delegated_now && !delegated_before_tx))
                            } else if delegated_now || delegated_before_tx {
                                23 * 1530
                            } else {
                                0
                            };
                            assert_eq!(
                                accounting.state_refund,
                                if state_gas { account + code } else { 0 }
                            );
                            assert_eq!(accounting.execution_refund, 0);
                        }
                    }
                }
            }
            let mut accounting = AuthRefunds::new(&version);
            accounting.rejected();
            assert_eq!(accounting.state_refund, if state_gas { 143 * 1530 } else { 0 });
            assert_eq!(accounting.execution_refund, 0);
        }
    }
}

fn set_cap<H: TxHandlerHooks<BaseEvmTypes> + 'static>(evm: &mut Evm<'_, BaseEvmTypes>, cap: u64) {
    let mut version = *evm.version();
    version.tx_gas_limit_cap = cap;
    let mut registry = TxRegistry::new();
    registry.register(
        4,
        TxEnvelope::as_eip7702,
        handler(
            eip7702::prepare_with_hooks::<BaseEvmTypes, H>,
            eip7702::execute_prepared::<BaseEvmTypes, H>,
        ),
    );
    evm.set_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        registry,
        Precompiles::base(SpecId::PRAGUE),
    );
}

#[test]
fn tip1143_t42_post_adjustment_execution_cap_has_exact_neighbor_controls() {
    for enabled in [false, true] {
        for floor in [false, true] {
            let required = if floor { 60_000 } else { 50_000 };
            for cap in [required - 1, required, required + 1] {
                let (mut execution, tx) = if floor {
                    let (mut evm, tx) =
                        fixture::<AdjustedFloor>(enabled, SpecId::PRAGUE, true, 70_000);
                    set_cap::<AdjustedFloor>(&mut evm, cap);
                    (evm, tx)
                } else {
                    let (mut evm, tx) =
                        fixture::<AdjustedIntrinsic>(enabled, SpecId::PRAGUE, true, 70_000);
                    set_cap::<AdjustedIntrinsic>(&mut evm, cap);
                    (evm, tx)
                };
                let validation = execution.validate_tx(&tx);
                let result = execution.transact(&tx).map(|result| result.detach());
                if cap < required {
                    let expected =
                        HandlerError::TxGasLimitGreaterThanCap { gas_limit: required, cap };
                    assert_eq!(validation, Err(expected.clone()));
                    assert_eq!(result.unwrap_err(), expected);
                    CALLS.with(|calls| assert_eq!(calls.get(), 0));
                } else {
                    assert!(validation.is_ok());
                    let detached = result.unwrap();
                    assert!(detached.result.status);
                    assert_eq!(
                        detached
                            .pending_state
                            .account_info(&Address::repeat_byte(0xcc))
                            .unwrap()
                            .nonce,
                        1
                    );
                    CALLS.with(|calls| assert_eq!(calls.get(), 1));
                }
            }
        }
        // Without EIP-8037 the cap applies to the full transaction budget,
        // regardless of a hook's smaller execution requirement.
        for budget in [59_999, 60_000, 60_001] {
            let (mut execution, tx) = fixture::<Delegating>(enabled, SpecId::PRAGUE, false, budget);
            set_cap::<Delegating>(&mut execution, 60_000);
            let result = execution.transact(&tx).map(|result| result.discard());
            if budget > 60_000 {
                assert_eq!(
                    result.unwrap_err(),
                    HandlerError::TxGasLimitGreaterThanCap { gas_limit: budget, cap: 60_000 }
                );
                CALLS.with(|calls| assert_eq!(calls.get(), 0));
            } else {
                assert!(result.unwrap().status);
                CALLS.with(|calls| assert_eq!(calls.get(), 1));
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct AuthorizationReads {
    accounts: Rc<RefCell<Vec<Address>>>,
    payloads: Rc<RefCell<usize>>,
}

impl Database for AuthorizationReads {
    type Error = io::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.accounts.borrow_mut().push(*address);
        Ok((*address == Address::repeat_byte(0xbb))
            .then(|| AccountInfo { balance: U256::from(u64::MAX), ..Default::default() }))
    }

    fn get_code_kind_by_hash(
        &mut self,
        _: &B256,
    ) -> Result<evm2::bytecode::BytecodeKind, Self::Error> {
        Ok(evm2::bytecode::BytecodeKind::Legacy)
    }

    fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
        *self.payloads.borrow_mut() += 1;
        Err(io::Error::other("unexpected authorization payload read"))
    }

    fn get_code_chunk_by_hash(
        &mut self,
        _: &B256,
        _: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        *self.payloads.borrow_mut() += 1;
        Err(io::Error::other("unexpected authorization chunk read"))
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

#[test]
fn tip1143_t42_authorization_oog_stops_before_later_authorities_and_recipient_reads() {
    let authorities =
        [Address::repeat_byte(0xcc), Address::repeat_byte(0xcd), Address::repeat_byte(0xce)];
    // Amsterdam without state gas: three AUTH_BASE intrinsic charges, followed
    // by one ACCOUNT_WRITE per distinct authority. Fail while charging the
    // second authority, after the first delegation has actually been applied.
    let intrinsic = 12_000 + 3_000 + 3 * 7_816;
    for enabled in [false, true] {
        for writes in [9_000 - 1, 18_000 - 1, 27_000 - 1, 27_000, 27_000 + 1] {
            CALLS.with(|calls| calls.set(0));
            let db = AuthorizationReads::default();
            let reads = db.clone();
            let mut version = Version::new(SpecId::AMSTERDAM).with_tip1143(enabled);
            version.features.set(EvmFeatures::EIP8037, false);
            let mut registry = TxRegistry::new();
            registry.register(
                4,
                TxEnvelope::as_eip7702,
                handler(
                    eip7702::prepare_with_hooks::<BaseEvmTypes, Delegating>,
                    eip7702::execute_prepared::<BaseEvmTypes, Delegating>,
                ),
            );
            let tx = Recovered::new_unchecked(
                TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
                    TxEip7702 {
                        chain_id: version.chain_id,
                        to: Address::repeat_byte(0xaa),
                        gas_limit: intrinsic + writes,
                        ..Default::default()
                    },
                    authorities
                        .iter()
                        .map(|authority| {
                            RecoveredAuthorization::new_unchecked(
                                Authorization {
                                    chain_id: U256::ZERO,
                                    address: Address::repeat_byte(0xdd),
                                    nonce: 0,
                                },
                                RecoveredAuthority::Valid(*authority),
                            )
                        })
                        .collect(),
                )),
                Address::repeat_byte(0xbb),
            );
            let mut evm = Evm::new_with_execution_config(
                ExecutionConfig::for_spec_and_version(SpecId::AMSTERDAM, version),
                SpecId::AMSTERDAM,
                BlockEnvExt::default(),
                registry,
                Db::new(db),
                Precompiles::base(SpecId::AMSTERDAM),
            );
            let detached = evm.transact(&tx).unwrap().detach();
            let succeeds = writes >= 27_000;
            assert_eq!(detached.result.status, succeeds, "enabled={enabled}, writes={writes}");
            assert_eq!(detached.result.total_gas_spent, intrinsic + writes.min(27_000));
            assert_eq!(detached.result.state_gas_spent, 0);
            assert_eq!(detached.result.refunded, 0);
            assert!(detached.result.logs.is_empty());
            CALLS.with(|calls| assert_eq!(calls.get(), 1));
            let ledger = reads.accounts.borrow();
            let reached = (writes / 9_000 + 1).min(3) as usize;
            let observed = ledger
                .iter()
                .copied()
                .filter(|address| authorities.contains(address))
                .collect::<Vec<_>>();
            assert_eq!(observed, authorities[..reached]);
            assert_eq!(
                ledger.iter().filter(|address| **address == Address::repeat_byte(0xaa)).count(),
                usize::from(succeeds)
            );
            assert_eq!(*reads.payloads.borrow(), 0);
            for authority in authorities {
                let info = detached.pending_state.account_info(&authority);
                if succeeds {
                    let info = info.unwrap();
                    assert_eq!(info.nonce, 1);
                    assert_eq!(
                        info.code_hash,
                        Bytecode::new_eip7702(Address::repeat_byte(0xdd)).hash_slow()
                    );
                    assert!(info.code_metadata.is_none());
                } else {
                    assert!(info.is_none(), "partial authorization must roll back");
                }
            }
        }
    }
}

/// Exercise genuine lazy signature recovery as well as consensus-envelope conversion.
#[test]
fn tip1143_t31_signed_and_recovered_authorizations_preserve_rejection_and_repetition() {
    let delegate = Address::repeat_byte(0xdd);
    // A low-s recoverable signature defines a deterministic authority for this
    // message. No secret key or external signing service is involved.
    let signed = SignedAuthorization::new_unchecked(
        Authorization { chain_id: U256::ZERO, address: delegate, nonce: 0 },
        0,
        U256::from(1),
        U256::from(1),
    );
    let authority = signed.recover_authority().unwrap();
    let invalid = SignedAuthorization::new_unchecked(
        Authorization { chain_id: U256::ZERO, address: delegate, nonce: 0 },
        2, // Invalid recovery parity must be rejected by either representation.
        U256::from(1),
        U256::from(1),
    );
    assert!(invalid.recover_authority().is_err());
    for enabled in [false, true] {
        for recovered in [false, true] {
            for (entries, applied) in [
                (vec![signed.clone()], true),
                (vec![invalid.clone()], false),
                (vec![signed.clone(), signed.clone()], true),
                (vec![invalid.clone(), signed.clone()], true),
            ] {
                let count = entries.len() as u64;
                let (mut execution, _) =
                    fixture::<Delegating>(enabled, SpecId::PRAGUE, false, 200_000);
                let consensus = TxEip7702 {
                    chain_id: execution.version().chain_id,
                    to: Address::repeat_byte(0xaa),
                    gas_limit: 200_000,
                    authorization_list: entries,
                    ..Default::default()
                };
                let lazy = if recovered {
                    LazyTxEip7702::from_recovered_authorizations(consensus)
                } else {
                    LazyTxEip7702::from_signed_authorizations(consensus)
                };
                let tx =
                    Recovered::new_unchecked(TxEnvelope::Eip7702(lazy), Address::repeat_byte(0xbb));
                let result = execution.transact(&tx).unwrap().detach();
                assert!(result.result.status);
                assert_eq!(result.result.total_gas_spent, 21000 + 25000 * count);
                assert_eq!(result.result.refunded, 0);
                assert_eq!(result.result.state_gas_spent, 0);
                assert!(result.result.logs.is_empty());
                assert_eq!(CALLS.with(Cell::get), 1);
                if applied {
                    let info = result.pending_state.account_info(&authority).unwrap();
                    assert_eq!(info.nonce, 1);
                    assert_eq!(info.code_hash, Bytecode::new_eip7702(delegate).hash_slow());
                    assert!(info.code_metadata.is_none());
                } else {
                    assert!(result.pending_state.account_info(&authority).is_none());
                }
            }
        }
    }
}
