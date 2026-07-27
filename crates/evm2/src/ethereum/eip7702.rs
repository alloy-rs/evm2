use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_gas_and_reservoir,
    initial_message, intrinsic_gas, settle_gas, validate_block_gas_limit, validate_chain_id,
    validate_create_initcode, validate_floor_gas, validate_gas_price, validate_intrinsic_gas,
    validate_nonce_not_overflow, validate_priority_fee, validate_regular_gas_limit_cap,
    validate_sender, validate_tx_gas_limit_cap, warm_access_list, warm_base_accounts,
};
use crate::{
    Evm, EvmFeatures, EvmTypes, TxResult,
    env::TxEnv,
    evm::error_handler,
    interpreter::{GasTracker, Host, InstrStop, MessageResult, gas::EIP8038_ACCOUNT_WRITE},
    registry::{HandlerError, HandlerResult, TxRequest},
    version::GasId,
};
use alloc::vec::Vec;
use alloy_primitives::{Address, U256};

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
    // EIP-2780 (ethereum/EIPs#11844): the per-auth state-dependent charges are applied at the
    // runtime gas phase, so no state gas is charged at the intrinsic phase (pre-Amsterdam there is
    // none either). A transaction that passes the intrinsic check but cannot afford the runtime
    // charges is included as an out-of-gas halt rather than rejected.
    validate_intrinsic_gas(tx.gas_limit, intrinsic, 0)?;
    let floor_gas = floor_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
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
    let tx_env =
        TxEnv { origin: caller, gas_price, chain_id: U256::from(chain_id), ..TxEnv::default() };

    if req.host.feature(EvmFeatures::EIP2780) {
        // EIP-2780 runtime gas phase (ethereum/EIPs#11844): the authorization charges are metered on
        // a transaction-level gas tracker as the delegations are applied, stopping at the first
        // unaffordable charge — later authorities are never loaded, keeping them out of the EIP-7928
        // block access list. The delegations span `auth_checkpoint` so a runtime out-of-gas can drop
        // them, and the recipient is read only afterwards (at first-frame creation), so it too stays
        // out of the block access list on an authorization out-of-gas.
        let (regular_gas_limit, reservoir) =
            initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, 0, 0);
        let mut tx_gas =
            GasTracker::new_with_regular_gas_and_reservoir(regular_gas_limit, reservoir);
        let auth_checkpoint = req.host.state.checkpoint();

        // Includes the transaction as an out-of-gas halt when the runtime gas phase (the
        // authorization charges or the first-frame recipient charge) runs out of gas: reverts the
        // authorization checkpoint to drop the applied delegations, then consumes all regular gas
        // and returns the reservoir. Called at either out-of-gas exit.
        let tx_gas_limit = tx.gas_limit;
        let oog_halt = |host: &mut Evm<'_, T>| -> HandlerResult<TxResult<T>> {
            let features = host.version().features;
            host.state.rollback(auth_checkpoint, features);
            let result = MessageResult {
                stop: InstrStop::OutOfGas,
                gas: GasTracker::new_spent_with_reservoir(regular_gas_limit, reservoir),
                ..MessageResult::default()
            };
            settle_gas(host, caller, gas_price, tx_gas_limit, floor_gas, 0, 0, result)
        };

        if apply_auth_list_runtime(
            req.host,
            chain_id,
            &tx.authorization_list,
            caller,
            tx.to,
            tx.value,
            &mut tx_gas,
        )? {
            return oog_halt(req.host);
        }

        // State gas charged for the authorizations, carried into the block state-gas accounting.
        let auth_state_gas = tx_gas.state_gas_spent().max(0) as u64;
        let (bytecode, mut message) = initial_message(
            req.host,
            caller,
            tx.nonce,
            tx.to.into(),
            &tx.input,
            tx.value,
            tx_gas.remaining(),
            tx_gas.reservoir(),
        )?;
        // Failed execution has already been rolled back to the message's own checkpoint (past the
        // applied delegations, which stay) inside `execute_message`.
        let result = req.host.execute_message(&tx_env, bytecode, &mut message);

        // A depth-0 recipient charge that ran out of gas is part of the runtime gas phase, so it
        // drops the delegations too.
        if result.runtime_gas_oog {
            return oog_halt(req.host);
        }

        return settle_gas(
            req.host,
            caller,
            gas_price,
            tx.gas_limit,
            floor_gas,
            auth_state_gas,
            0,
            result,
        );
    }

    // Pre-Amsterdam (no EIP-2780): the pessimistic per-auth intrinsic charge with a regular refund
    // for each already-existing authority.
    let (state_refund, regular_refund) =
        apply_auth_list(req.host, chain_id, &tx.authorization_list)?;
    let (gas_limit, reservoir) =
        initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, 0, state_refund);
    let (bytecode, mut message) = initial_message(
        req.host,
        caller,
        tx.nonce,
        tx.to.into(),
        &tx.input,
        tx.value,
        gas_limit,
        reservoir,
    )?;
    // Failed execution has already been rolled back to the message's own checkpoint inside
    // `execute_message`.
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message);
    result.gas.set_refunded(
        result.gas.refunded().saturating_add(i64::try_from(regular_refund).unwrap_or(i64::MAX)),
    );

    settle_gas(req.host, caller, gas_price, tx.gas_limit, floor_gas, 0, state_refund, result)
}

fn eip7702_authorization_gas<'a, T: EvmTypes>(host: &Evm<'a, T>, authorizations: usize) -> u64 {
    let per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    (authorizations as u64).saturating_mul(per_auth)
}

/// Outcome of validating one EIP-7702 authorization, carrying the facts needed to compute its gas
/// charges (execution-specs `set_delegation`).
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

/// Validates one authorization against current state without applying it. Returns
/// `Some((authority, facts))` for an accepted authorization or `None` for a rejected one. Mirrors
/// execution-specs `validate_authorization`.
fn validate_one_auth<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorization: &super::LazyAuthorization,
) -> HandlerResult<Option<(Address, AppliedAuth)>> {
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
    Ok(Some((authority, AppliedAuth { existed, delegated_before_tx, delegated_now, clearing })))
}

/// Validates and, if accepted, applies one authorization (setting code and bumping the nonce).
/// Returns `Some` for an accepted authorization or `None` for a rejected one.
fn apply_one_auth<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorization: &super::LazyAuthorization,
) -> HandlerResult<Option<AppliedAuth>> {
    let Some((authority, auth)) = validate_one_auth(host, chain_id, authorization)? else {
        return Ok(None);
    };
    host.state
        .account(&authority, false)
        .map_err(error_handler!(host))?
        .set_delegation(*authorization.address());
    Ok(Some(auth))
}

/// Applies the EIP-7702 authorization list under EIP-2780, metering the state-dependent charges on
/// the transaction-level `gas` as the delegations are applied (ethereum/EIPs#11844, #11891).
///
/// Per accepted authority: the new-account state gas when the authority does not exist,
/// `ACCOUNT_WRITE` regular gas on the first write to the authority's leaf (unless already paid — the
/// sender at inclusion, the recipient of a value-bearing transaction, or a preceding valid
/// authorization on the same authority), and the net-new delegation-indicator state gas.
///
/// The charges are recorded as the authorizations are applied, so the phase stops at the first
/// unaffordable charge without loading the remaining authorities. Rejected authorizations charge
/// nothing (the intrinsic `REGULAR_PER_AUTH_BASE_COST` already covers their work) and are not
/// refunded. Returns whether the authorization processing ran out of gas.
#[allow(clippy::too_many_arguments)]
fn apply_auth_list_runtime<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorizations: &[super::LazyAuthorization],
    caller: Address,
    recipient: Address,
    value: U256,
    gas: &mut GasTracker,
) -> HandlerResult<bool> {
    let new_account_state_gas = host.version().gas_params.new_account_state_gas();
    let delegation_bytes_state_gas =
        u64::from(host.version().gas_params.get(GasId::TxEip7702PerAuthState));
    let account_write_cost = u64::from(EIP8038_ACCOUNT_WRITE);

    // Accounts whose leaf write this transaction has already paid for: the sender at inclusion
    // (priced into `TX_BASE`) and the recipient of a value-bearing transaction (priced into
    // `TX_VALUE_COST`).
    let mut written = Vec::new();
    written.push(caller);
    if !value.is_zero() {
        written.push(recipient);
    }
    // Net-new delegation bytes are charged at most once per authority (covering a set-clear-set
    // sequence within one transaction).
    let mut charged_delegation_bytes: Vec<Address> = Vec::new();

    for authorization in authorizations {
        let Some((authority, auth)) = validate_one_auth(host, chain_id, authorization)? else {
            continue;
        };

        // Non-existent authority: pay for the new account leaf's state bytes.
        if !auth.existed && gas.spend_state(new_account_state_gas).is_err() {
            return Ok(true);
        }
        // First write to the authority's leaf within the transaction pays `ACCOUNT_WRITE`.
        if !written.contains(&authority) {
            if gas.spend(account_write_cost).is_err() {
                return Ok(true);
            }
            written.push(authority);
        }
        // Net-new delegation bytes: the 23-byte designator written into a previously empty slot.
        if !auth.clearing
            && !auth.delegated_now
            && !auth.delegated_before_tx
            && !charged_delegation_bytes.contains(&authority)
        {
            if gas.spend_state(delegation_bytes_state_gas).is_err() {
                return Ok(true);
            }
            charged_delegation_bytes.push(authority);
        }

        host.state
            .account(&authority, false)
            .map_err(error_handler!(host))?
            .set_delegation(*authorization.address());
    }

    Ok(false)
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
