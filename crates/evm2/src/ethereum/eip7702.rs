use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_message,
    intrinsic_gas, rollback_failed_execution, settle_gas, validate_block_gas_limit,
    validate_chain_id, validate_create_initcode, validate_floor_gas, validate_gas_price,
    validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
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

pub(super) fn handle<T>(
    req: TxRequest<'_, '_, T, Recovered<super::LazyTxEip7702>>,
) -> HandlerResult<TxResult<T>>
where
    T: EvmTypes,
    for<'a> T: EvmTypes<Host<'a> = Evm<'a, T>>,
{
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
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
    ) + eip7702_authorization_gas(req.host, tx.authorization_list.len());
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
    req.host.state.account(&caller, false).map_err(db_error_handler!(req.host))?.bump_nonce();
    let chain_id = req.host.version().chain_id;
    let eip7702_refund = apply_auth_list(req.host, chain_id, &tx.authorization_list)?;
    let execution_checkpoint = req.host.state.checkpoint();

    let gas_limit = tx.gas_limit - intrinsic;
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) =
        initial_message(req.host, caller, tx.nonce, tx.to.into(), &tx.input, tx.value, gas_limit)?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);
    result.gas.set_refunded(
        result.gas.refunded().saturating_add(i64::try_from(eip7702_refund).unwrap_or(i64::MAX)),
    );

    settle_gas(req.host, caller, gas_price, tx.gas_limit, floor_gas, result)
}

fn eip7702_authorization_gas<'a, T>(host: &Evm<'a, T>, authorizations: usize) -> u64
where
    T: EvmTypes<Host<'a> = Evm<'a, T>>,
{
    let per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    u64::try_from(authorizations).unwrap_or(u64::MAX).saturating_mul(per_auth)
}

fn apply_auth_list<'a, T>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorizations: &[super::LazyAuthorization],
) -> HandlerResult<u64>
where
    T: EvmTypes<Host<'a> = Evm<'a, T>>,
{
    let mut refunded_accounts = 0u64;
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
        if !code.is_empty() && !code.is_eip7702() {
            continue;
        }
        if authorization.nonce() != authority_nonce {
            continue;
        }

        if existed {
            refunded_accounts = refunded_accounts.saturating_add(1);
        }
        account.set_delegation(*authorization.address());
    }

    let refund_per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702AuthRefund));
    Ok(refunded_accounts.saturating_mul(refund_per_auth))
}
