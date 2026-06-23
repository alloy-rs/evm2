use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_gas_and_reservoir,
    initial_message, intrinsic_gas, rollback_failed_execution, settle_gas,
    validate_block_gas_limit, validate_chain_id, validate_create_initcode, validate_floor_gas,
    validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
    validate_regular_gas_limit_cap, validate_sender, validate_tx_gas_limit_cap, warm_access_list,
    warm_base_accounts,
};
use crate::{
    Evm, EvmTypes, TxResult,
    env::TxEnv,
    evm::db_error_handler,
    interpreter::Host,
    registry::{HandlerError, HandlerResult, TxRequest},
    version::GasId,
};
use alloy_consensus::transaction::Recovered;
use alloy_primitives::U256;

pub(super) fn handle<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, T, Recovered<super::LazyTxEip7702>>,
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
    // EIP-8037: per-auth state gas (account + bytecode) is charged before execution. Zero before
    // Amsterdam.
    let num_auths = u64::try_from(tx.authorization_list.len()).unwrap_or(u64::MAX);
    let initial_state_gas =
        num_auths.saturating_mul(req.host.version().gas_params.eip7702_auth_state_gas());
    validate_intrinsic_gas(tx.gas_limit, intrinsic, initial_state_gas)?;
    let floor_gas =
        floor_gas(req.host.version(), &tx.input, access_list_accounts, access_list_storage_keys);
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to.into());
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    charge_upfront(req.host, caller, effective_gas_cost)?;
    req.host.state.account(&caller, false).map_err(db_error_handler!(req.host))?.bump_nonce();
    let chain_id = req.host.version().chain_id;
    let (refunded_accounts, refunded_bytecodes) =
        apply_auth_list(req.host, chain_id, &tx.authorization_list)?;
    // EIP-8037: existing authorities / already-delegated bytecodes earn a state-gas refund (zero
    // before Amsterdam). The regular-gas refund per existing account (Prague: 12500; Amsterdam: 0)
    // is routed through the capped refund counter as before.
    let state_refund =
        req.host.version().gas_params.eip7702_state_refund(refunded_accounts, refunded_bytecodes);
    let regular_refund = refunded_accounts
        .saturating_mul(u64::from(req.host.version().gas_params.get(GasId::TxEip7702AuthRefund)));
    let execution_checkpoint = req.host.state.checkpoint();

    // EIP-7702 transactions are always calls, never creates.
    let (gas_limit, reservoir) = initial_gas_and_reservoir(
        req.host.version(),
        tx.gas_limit,
        intrinsic,
        initial_state_gas,
        state_refund,
    );
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
    )?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);
    result.gas.set_refunded(
        result.gas.refunded().saturating_add(i64::try_from(regular_refund).unwrap_or(i64::MAX)),
    );

    settle_gas(
        req.host,
        caller,
        gas_price,
        tx.gas_limit,
        floor_gas,
        initial_state_gas,
        state_refund,
        false,
        result,
    )
}

fn eip7702_authorization_gas<T: EvmTypes<Host = Evm<T>>>(
    host: &Evm<T>,
    authorizations: usize,
) -> u64 {
    let per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    u64::try_from(authorizations).unwrap_or(u64::MAX).saturating_mul(per_auth)
}

/// Applies the EIP-7702 authorization list and returns `(refunded_accounts, refunded_bytecodes)`:
/// the number of authorizations whose authority already existed, and the number whose authority
/// already carried delegation bytecode (or whose designation is being cleared). These drive the
/// EIP-7702 gas refunds (regular and, under EIP-8037, state).
fn apply_auth_list<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    chain_id: u64,
    authorizations: &[super::LazyAuthorization],
) -> HandlerResult<(u64, u64)> {
    let mut refunded_accounts = 0u64;
    let mut refunded_bytecodes = 0u64;
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
        let mut account = host.state.account(&authority, false).map_err(db_error_handler!(host))?;
        account.warm();
        let existed = account.exists();
        let authority_nonce = account.nonce();
        let code = account.load_code().map_err(db_error_handler!(host))?;
        // Past the filter below, non-empty code is necessarily an existing EIP-7702 delegation.
        let has_delegation_code = !code.is_empty();
        if has_delegation_code && !code.is_eip7702() {
            continue;
        }
        if authorization.nonce() != authority_nonce {
            continue;
        }

        if existed {
            refunded_accounts = refunded_accounts.saturating_add(1);
        }
        // Per-bytecode refund: the authority already held delegation bytecode, or the designation
        // is being cleared (target is the zero address).
        if has_delegation_code || authorization.address().is_zero() {
            refunded_bytecodes = refunded_bytecodes.saturating_add(1);
        }
        account.set_delegation(*authorization.address());
    }

    Ok((refunded_accounts, refunded_bytecodes))
}
