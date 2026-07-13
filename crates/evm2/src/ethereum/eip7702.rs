use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_gas_and_reservoir,
    initial_message, intrinsic_gas, rollback_failed_execution, settle_gas,
    validate_block_gas_limit, validate_chain_id, validate_create_initcode, validate_floor_gas,
    validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
    validate_regular_gas_limit_cap, validate_sender, validate_tx_gas_limit_cap, warm_access_list,
    warm_base_accounts,
};
use crate::{
    Evm, EvmFeatures, EvmTypes, TxResult,
    env::TxEnv,
    evm::error_handler,
    interpreter::{GasTracker, Host, InstrStop, MessageResult},
    registry::{HandlerError, HandlerResult, TxRequest},
    version::GasId,
};
use alloy_primitives::{Address, TxKind, U256, map::AddressHashSet};

/// Executes an EIP-7702 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(
    req: TxRequest<'_, '_, T, super::LazyTxEip7702>,
) -> HandlerResult<TxResult<T>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    if tx.authorization_list.is_empty() {
        return Err(HandlerError::EmptyAuthorizationList);
    }
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to.into(), &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let intrinsic = intrinsic_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    ) + eip7702_authorization_gas(req.host, tx.authorization_list.len());
    let is_eip2780 = req.host.feature(EvmFeatures::EIP2780);
    let pre_execution_state_gas = if is_eip2780 {
        0
    } else {
        eip7702_authorization_state_gas(req.host, tx.authorization_list.len())
    };
    validate_intrinsic_gas(tx.gas_limit, intrinsic, pre_execution_state_gas)?;
    let floor_gas = floor_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        tx.value,
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
    );
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to.into());
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    charge_upfront(req.host, caller, effective_gas_cost)?;
    req.host.state.account(&caller, false).map_err(error_handler!(req.host))?.bump_nonce();
    let chain_id = req.host.version().chain_id;

    let mut state_refund = 0;
    let mut regular_refund = 0;
    let settlement_initial_state_gas;
    let (gas_limit, reservoir) = if is_eip2780 {
        let (gas_limit, reservoir) =
            initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, 0, 0);
        let mut runtime_gas = GasTracker::new_with_regular_gas_and_reservoir(gas_limit, reservoir);
        let auth_checkpoint = req.host.state.checkpoint();
        let auth_gas = apply_auth_list_eip2780(
            req.host,
            chain_id,
            &tx.authorization_list,
            caller,
            tx.to.into(),
            tx.value,
            &mut runtime_gas,
        )?;
        if auth_gas.out_of_gas {
            req.host.state.rollback(auth_checkpoint, req.host.version().features);
            return settle_gas(
                req.host,
                caller,
                gas_price,
                tx.gas_limit,
                floor_gas,
                0,
                0,
                false,
                runtime_oog_result(tx.gas_limit, reservoir),
            );
        }
        settlement_initial_state_gas = auth_gas.state_gas;
        (runtime_gas.remaining(), runtime_gas.reservoir())
    } else {
        (state_refund, regular_refund) =
            apply_auth_list(req.host, chain_id, &tx.authorization_list)?;
        settlement_initial_state_gas = pre_execution_state_gas;
        initial_gas_and_reservoir(
            req.host.version(),
            tx.gas_limit,
            intrinsic,
            pre_execution_state_gas,
            state_refund,
        )
    };
    let execution_checkpoint = req.host.state.checkpoint();

    // EIP-7702 transactions are always calls, never creates.
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) = initial_message(
        req.host,
        caller,
        tx.nonce,
        tx.to.into(),
        &tx.input,
        tx.value,
        gas_limit,
        reservoir,
        0,
    )?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);
    if regular_refund != 0 {
        result.gas.set_refunded(
            result.gas.refunded().saturating_add(i64::try_from(regular_refund).unwrap_or(i64::MAX)),
        );
    }

    settle_gas(
        req.host,
        caller,
        gas_price,
        tx.gas_limit,
        floor_gas,
        settlement_initial_state_gas,
        state_refund,
        false,
        result,
    )
}

fn eip7702_authorization_gas<'a, T: EvmTypes>(host: &Evm<'a, T>, authorizations: usize) -> u64 {
    let mut per_auth =
        u64::from(host.version().gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    if host.feature(EvmFeatures::EIP2780) {
        per_auth = per_auth
            .saturating_sub(u64::from(host.version().gas_params.get(GasId::TxEip7702AuthRefund)));
    }
    (authorizations as u64).saturating_mul(per_auth)
}

/// EIP-8037 per-authorization state gas (account + bytecode) charged before execution. Zero before
/// Amsterdam.
const fn eip7702_authorization_state_gas<'a, T: EvmTypes>(
    host: &Evm<'a, T>,
    authorizations: usize,
) -> u64 {
    (authorizations as u64).saturating_mul(host.version().gas_params.eip7702_auth_state_gas())
}

/// Outcome of applying one accepted EIP-7702 authorization, carrying the facts needed to compute
/// its gas refunds (execution-specs `set_delegation`).
struct AppliedAuth {
    /// Whether the authority account already existed when this authorization was processed.
    existed: bool,
    /// Whether the authority's code was a valid delegation at the start of the transaction.
    delegated_before_tx: bool,
    /// Whether the authority's code was a valid delegation when this authorization was processed
    /// (i.e. as left by an earlier authorization for the same authority in this transaction).
    delegated_now: bool,
    /// Whether this authorization clears the delegation (target is the zero address).
    clearing: bool,
}

/// Validates one authorization against current state and, if accepted, applies the delegation
/// (setting code and bumping the nonce). Returns `Some` for an accepted authorization or `None` for
/// a rejected one. Mirrors execution-specs `validate_authorization` + the per-auth body of
/// `set_delegation`.
fn apply_one_auth<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorization: &super::LazyAuthorization,
) -> HandlerResult<Option<AppliedAuth>> {
    if !authorization.chain_id().is_zero() && authorization.chain_id() != &U256::from(chain_id) {
        return Ok(None);
    }
    if authorization.nonce() == u64::MAX {
        return Ok(None);
    }
    let Some(authority) = authorization.authority() else {
        return Ok(None);
    };
    let mut account = host.state.account(&authority, false).map_err(error_handler!(host))?;
    account.warm();
    let existed = account.exists();
    let authority_nonce = account.nonce();
    let code = account.load_code().map_err(error_handler!(host))?;
    // Reject an authority that already carries non-delegation code; otherwise non-empty code is
    // necessarily a valid delegation.
    let delegated_now = !code.is_empty();
    if delegated_now && !code.is_eip7702() {
        return Ok(None);
    }
    if authorization.nonce() != authority_nonce {
        return Ok(None);
    }
    let delegated_before_tx = account.original_code().map_err(error_handler!(host))?.is_eip7702();
    let clearing = authorization.address().is_zero();
    account.set_delegation(*authorization.address());
    Ok(Some(AppliedAuth { existed, delegated_before_tx, delegated_now, clearing }))
}

#[derive(Clone, Copy, Debug, Default)]
struct RuntimeAuthListGas {
    state_gas: u64,
    out_of_gas: bool,
}

fn runtime_oog_result<T: EvmTypes>(tx_gas_limit: u64, reservoir: u64) -> MessageResult<T> {
    MessageResult {
        stop: InstrStop::OutOfGas,
        gas: GasTracker::new_spent_with_reservoir(tx_gas_limit, reservoir),
        ..MessageResult::default()
    }
}

const fn spend_regular(gas: &mut GasTracker, cost: u64) -> bool {
    let mut next = *gas;
    if next.spend(cost).is_err() {
        return false;
    }
    *gas = next;
    true
}

fn spend_state(gas: &mut GasTracker, cost: u64) -> bool {
    let mut next = *gas;
    if next.spend_state(cost).is_err() {
        return false;
    }
    *gas = next;
    true
}

fn apply_auth_list_eip2780<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorizations: &[super::LazyAuthorization],
    caller: Address,
    to: TxKind,
    value: U256,
    gas: &mut GasTracker,
) -> HandlerResult<RuntimeAuthListGas> {
    let account_write = u64::from(host.version().gas_params.get(GasId::TxEip7702AuthRefund));
    let new_account = host.version().gas_params.new_account_state_gas();
    let auth_base = u64::from(host.version().gas_params.get(GasId::TxEip7702PerAuthState));
    let mut written_accounts = AddressHashSet::default();
    written_accounts.insert(caller);
    if let TxKind::Call(target) = to
        && !value.is_zero()
    {
        written_accounts.insert(target);
    }
    let mut charged_delegation_bytes = AddressHashSet::default();
    let mut state_gas = 0u64;

    for authorization in authorizations {
        if !authorization.chain_id().is_zero() && authorization.chain_id() != &U256::from(chain_id)
        {
            continue;
        }
        if authorization.nonce() == u64::MAX {
            continue;
        }
        let Some(authority) = authorization.authority() else {
            continue;
        };

        let mut account = host.state.account(&authority, false).map_err(error_handler!(host))?;
        account.warm();
        let existed = account.exists();
        let authority_nonce = account.nonce();
        let code = account.load_code().map_err(error_handler!(host))?;
        let delegated_now = !code.is_empty();
        if delegated_now && !code.is_eip7702() {
            continue;
        }
        if authorization.nonce() != authority_nonce {
            continue;
        }
        let delegated_before_tx =
            account.original_code().map_err(error_handler!(host))?.is_eip7702();
        let clearing = authorization.address().is_zero();

        if !existed {
            if !spend_state(gas, new_account) {
                return Ok(RuntimeAuthListGas { state_gas, out_of_gas: true });
            }
            state_gas = state_gas.saturating_add(new_account);
        }
        if !written_accounts.contains(&authority) {
            if !spend_regular(gas, account_write) {
                return Ok(RuntimeAuthListGas { state_gas, out_of_gas: true });
            }
            written_accounts.insert(authority);
        }
        if !clearing
            && !delegated_now
            && !delegated_before_tx
            && !charged_delegation_bytes.contains(&authority)
        {
            if !spend_state(gas, auth_base) {
                return Ok(RuntimeAuthListGas { state_gas, out_of_gas: true });
            }
            state_gas = state_gas.saturating_add(auth_base);
            charged_delegation_bytes.insert(authority);
        }

        account.set_delegation(*authorization.address());
    }

    Ok(RuntimeAuthListGas { state_gas, out_of_gas: false })
}

/// Applies the EIP-7702 authorization list and returns `(state_refund, regular_refund)`.
///
/// Follows execution-specs `set_delegation`. The per-authorization state and regular gas charged in
/// the intrinsic cost is refilled when it turns out not to be needed: the state refund is credited
/// to the reservoir (so it stays state gas) and the regular refund is routed through the capped
/// refund counter.
///
/// Before EIP-8037 (Prague) there is no state gas: only the per-existing-account regular refund
/// applies and rejected authorizations refund nothing.
fn apply_auth_list<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorizations: &[super::LazyAuthorization],
) -> HandlerResult<(u64, u64)> {
    let is_eip8037 = host.feature(EvmFeatures::EIP8037);
    let new_account = host.version().gas_params.new_account_state_gas();
    let auth_base = u64::from(host.version().gas_params.get(GasId::TxEip7702PerAuthState));
    let regular_per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702AuthRefund));

    let mut state_refund = 0u64;
    let mut regular_refund = 0u64;
    for authorization in authorizations {
        let Some(auth) = apply_one_auth(host, chain_id, authorization)? else {
            // Rejected authorization. Under EIP-8037 its full intrinsic state gas (account +
            // bytecode) refills the reservoir and the speculative account write is refunded;
            // before EIP-8037 nothing is refunded.
            if is_eip8037 {
                state_refund = state_refund.saturating_add(new_account + auth_base);
                regular_refund = regular_refund.saturating_add(regular_per_auth);
            }
            continue;
        };

        // Existing authority: the worst-case `ACCOUNT_WRITE` regular gas was not needed. This
        // refund applies in every regime (it is the only authorization refund before EIP-8037).
        if auth.existed {
            regular_refund = regular_refund.saturating_add(regular_per_auth);
        }

        // The remaining refunds are state gas, which only exists under EIP-8037.
        if !is_eip8037 {
            continue;
        }

        let mut refund = 0u64;
        // Existing authority: its `NEW_ACCOUNT` state gas was not needed.
        if auth.existed {
            refund += new_account;
        }
        // Bytecode (`AUTH_BASE`) refunds.
        if auth.clearing {
            refund += auth_base;
            // Clearing a delegation freshly installed earlier in this transaction refills the
            // bytecode state gas a second time.
            if auth.delegated_now && !auth.delegated_before_tx {
                refund += auth_base;
            }
        } else if auth.delegated_now || auth.delegated_before_tx {
            refund += auth_base;
        }
        state_refund = state_refund.saturating_add(refund);
    }

    Ok((state_refund, regular_refund))
}
