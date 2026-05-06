use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_message,
    intrinsic_gas, rollback_failed_execution, settle_gas, validate_block_gas_limit,
    validate_create_initcode, validate_floor_gas, validate_gas_price, validate_intrinsic_gas,
    validate_nonce_not_overflow, validate_priority_fee, validate_sender, validate_tx_gas_limit_cap,
    warm_access_list, warm_base_accounts,
};
use crate::{
    Evm, EvmTypes, SpecId, TxResult,
    env::TxEnv,
    interpreter::Host,
    registry::{HandlerError, HandlerResult, TxRequest},
};
use alloy_consensus::{TxEip1559, transaction::Recovered};
use alloy_primitives::U256;

pub(super) fn handle<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, Recovered<TxEip1559>, Evm<T>>,
) -> HandlerResult<TxResult> {
    let spec_id = req.host.spec_id();
    if !spec_id.enables(SpecId::LONDON) {
        return Err(HandlerError::Eip1559NotSupported);
    }

    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);

    validate_priority_fee(max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(spec_id, gas_price, req.host.block.basefee)?;
    validate_tx_gas_limit_cap(spec_id, tx.gas_limit)?;
    validate_block_gas_limit(tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(spec_id, tx.to, &tx.input)?;
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
    let floor_gas = floor_gas(req.host.version(), &tx.input);
    validate_floor_gas(tx.gas_limit, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, spec_id, caller, tx.to);
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    charge_upfront(req.host, caller, effective_gas_cost);
    req.host.state.increment_nonce(caller);
    let execution_checkpoint = req.host.state.checkpoint();

    let gas_limit = tx.gas_limit - intrinsic;
    let tx_env =
        TxEnv { origin: caller, gas_price, chain_id: U256::from(tx.chain_id), ..TxEnv::default() };
    let (bytecode, message) =
        initial_message(req.host, caller, tx.nonce, tx.to, &tx.input, tx.value, gas_limit);
    let mut result = req.host.execute_message(&tx_env, bytecode, &message, false);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);

    Ok(settle_gas(req.host, spec_id, caller, gas_price, tx.gas_limit, floor_gas, result))
}
