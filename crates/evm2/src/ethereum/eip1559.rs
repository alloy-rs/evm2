use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_execution_gas,
    initial_message, intrinsic_gas, intrinsic_state_gas, rollback_failed_execution, settle_gas,
    validate_block_gas_limit, validate_chain_id, validate_create_initcode, validate_floor_gas,
    validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
    validate_regular_gas_limit_cap, validate_sender, validate_tx_gas_limit_cap, warm_access_list,
    warm_base_accounts,
};
use crate::{
    Evm, EvmTypes, TxResult,
    env::TxEnv,
    interpreter::Host,
    registry::{HandlerResult, TxRequest},
};
use alloy_consensus::{TxEip1559, transaction::Recovered};
use alloy_primitives::U256;

pub(super) fn handle<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, Recovered<TxEip1559>, Evm<T>>,
) -> HandlerResult<TxResult<T>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to, &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let intrinsic = intrinsic_gas(
        req.host.version(),
        tx.to,
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
    );
    validate_intrinsic_gas(tx.gas_limit, intrinsic)?;
    let floor_gas =
        floor_gas(req.host.version(), &tx.input, access_list_accounts, access_list_storage_keys);
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to);
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    charge_upfront(req.host, caller, effective_gas_cost)?;
    req.host.state.increment_nonce(&caller).map_err(|code| req.host.db_error_handler(code))?;
    let execution_checkpoint = req.host.state.checkpoint();

    let intrinsic_state = intrinsic_state_gas(req.host.version(), tx.to);
    let (gas_limit, gas_reservoir) =
        initial_execution_gas(req.host.version(), tx.gas_limit, intrinsic, intrinsic_state);
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) =
        initial_message(req.host, caller, tx.nonce, tx.to, &tx.input, tx.value, gas_limit)?;
    message.gas_reservoir = gas_reservoir;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message, false);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);

    settle_gas(
        req.host,
        caller,
        gas_price,
        tx.gas_limit,
        floor_gas,
        intrinsic,
        intrinsic_state,
        intrinsic_state,
        result,
    )
}
