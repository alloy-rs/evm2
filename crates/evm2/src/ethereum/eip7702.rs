use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas,
    initial_execution_gas_with_state_refund, initial_message, intrinsic_gas,
    rollback_failed_execution, settle_gas, validate_block_gas_limit, validate_chain_id,
    validate_create_initcode, validate_floor_gas, validate_gas_price, validate_intrinsic_gas,
    validate_nonce_not_overflow, validate_priority_fee, validate_regular_gas_limit_cap,
    validate_sender, validate_tx_gas_limit_cap, warm_access_list, warm_base_accounts,
};
use crate::{
    Evm, EvmTypes, TxResult,
    bytecode::Bytecode,
    env::TxEnv,
    interpreter::Host,
    registry::{HandlerError, HandlerResult, TxRequest},
    version::{EvmFeatures, GasId, Version},
};
use alloy_consensus::{TxEip7702, transaction::Recovered};
use alloy_eips::eip7702::SignedAuthorization;
use alloy_primitives::{Address, U256};

pub(super) fn handle<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, Recovered<TxEip7702>, Evm<T>>,
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
    let auth_state_gas = eip7702_authorization_state_gas(req.host, tx.authorization_list.len());
    let intrinsic =
        intrinsic_gas(
            req.host.version(),
            tx.to.into(),
            &tx.input,
            access_list_accounts,
            access_list_storage_keys,
        ) + eip7702_authorization_regular_gas(req.host.version(), tx.authorization_list.len())
            + auth_state_gas;
    validate_intrinsic_gas(tx.gas_limit, intrinsic)?;
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
    req.host.state.increment_nonce(&caller).map_err(|code| req.host.db_error_handler(code))?;
    let chain_id = req.host.version().chain_id;
    let eip7702_refund = apply_auth_list(req.host, chain_id, &tx.authorization_list)?;
    let (eip7702_state_refund, eip7702_regular_refund) =
        split_eip7702_refund(req.host, eip7702_refund);
    let execution_checkpoint = req.host.state.checkpoint();

    let (gas_limit, gas_reservoir) = initial_execution_gas_with_state_refund(
        req.host.version(),
        tx.gas_limit,
        intrinsic.saturating_sub(auth_state_gas),
        auth_state_gas,
        eip7702_state_refund,
    );
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) =
        initial_message(req.host, caller, tx.nonce, tx.to.into(), &tx.input, tx.value, gas_limit)?;
    message.gas_reservoir = gas_reservoir;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message, false);
    if eip7702_regular_refund > 0 {
        result.gas.record_refund(i64::try_from(eip7702_regular_refund).unwrap_or(i64::MAX));
    }
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);

    let intrinsic_state = auth_state_gas.saturating_sub(eip7702_state_refund);
    let failure_intrinsic_state_refund =
        if tx.gas_limit > req.host.version().tx_gas_limit_cap { intrinsic_state } else { 0 };
    settle_gas(
        req.host,
        caller,
        gas_price,
        tx.gas_limit,
        floor_gas,
        intrinsic,
        intrinsic_state,
        failure_intrinsic_state_refund,
        result,
    )
}

fn eip7702_authorization_regular_gas(version: &Version, authorizations: usize) -> u64 {
    let per_empty_account = u64::from(version.gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    let per_auth_state = u64::from(version.gas_params.get(GasId::TxEip7702PerAuthState));
    let per_auth_regular = per_empty_account.saturating_sub(per_auth_state);
    u64::try_from(authorizations).unwrap_or(u64::MAX).saturating_mul(per_auth_regular)
}

fn eip7702_authorization_state_gas<T: EvmTypes<Host = Evm<T>>>(
    host: &Evm<T>,
    authorizations: usize,
) -> u64 {
    let per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702PerAuthState));
    u64::try_from(authorizations).unwrap_or(u64::MAX).saturating_mul(per_auth)
}

fn split_eip7702_refund<T: EvmTypes<Host = Evm<T>>>(host: &Evm<T>, refund: u64) -> (u64, u64) {
    if host.version().feature(EvmFeatures::EIP8037) {
        return (refund, 0);
    }

    let per_auth_refund = u64::from(host.version().gas_params.get(GasId::TxEip7702AuthRefund));
    let per_auth_state = u64::from(host.version().gas_params.get(GasId::TxEip7702PerAuthState));
    if per_auth_refund == 0 || per_auth_state == 0 || refund == 0 {
        return (0, refund);
    }

    let state_refund_per_auth = per_auth_refund.min(per_auth_state);
    let refunded_auths = refund / per_auth_refund;
    let state_refund = refunded_auths.saturating_mul(state_refund_per_auth);
    (state_refund, refund.saturating_sub(state_refund))
}

fn apply_auth_list<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    chain_id: u64,
    authorizations: &[SignedAuthorization],
) -> HandlerResult<u64> {
    let mut state_refund = 0u64;
    for authorization in authorizations {
        if !authorization.chain_id().is_zero() && authorization.chain_id() != &U256::from(chain_id)
        {
            continue;
        }
        if authorization.nonce() == u64::MAX {
            continue;
        }

        let Ok(authority) = authorization.recover_authority() else {
            continue;
        };
        host.state.warm_account_non_revertible(&authority);
        let authority_info =
            host.state.account_info(&authority).map_err(|code| host.db_error_handler(code))?;
        let existed = authority_info.is_some();
        let authority_info = authority_info.unwrap_or_default();
        let code = host.state.get_code(&authority).map_err(|code| host.db_error_handler(code))?;
        if !code.is_empty() && !code.is_eip7702() {
            continue;
        }
        if authorization.nonce() != authority_info.nonce {
            continue;
        }

        host.state.record_account_access(&authority);
        if existed {
            state_refund = state_refund.saturating_add(u64::from(
                host.version().gas_params.get(GasId::TxEip7702AuthRefund),
            ));
        }
        if !code.is_empty() || authorization.address().is_zero() {
            let auth_state = u64::from(host.version().gas_params.get(GasId::TxEip7702PerAuthState));
            let account_state = u64::from(host.version().gas_params.get(GasId::NewAccountState));
            state_refund = state_refund.saturating_add(auth_state.saturating_sub(account_state));
        }
        set_delegation(host, authority, *authorization.address())?;
    }

    Ok(state_refund)
}

fn set_delegation<T: EvmTypes<Host = Evm<T>>>(
    host: &mut Evm<T>,
    authority: Address,
    delegated_address: Address,
) -> HandlerResult<()> {
    let code = if delegated_address.is_zero() {
        Bytecode::default()
    } else {
        Bytecode::new_eip7702(delegated_address)
    };
    host.state.set_code(&authority, code).map_err(|code| host.db_error_handler(code))?;
    host.state.increment_nonce(&authority).map_err(|code| host.db_error_handler(code))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpecId;

    #[test]
    fn eip7702_prague_charges_full_empty_account_cost_upfront() {
        let version = Version::base(SpecId::PRAGUE);

        assert_eq!(eip7702_authorization_regular_gas(version, 2), 50_000);
    }

    #[test]
    fn eip7702_amsterdam_splits_regular_and_state_auth_cost() {
        let version = Version::base(SpecId::AMSTERDAM);

        assert_eq!(eip7702_authorization_regular_gas(version, 2), 15_000);
    }
}
