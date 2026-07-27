use super::{
    access_list_counts, charge_upfront, effective_gas_price, floor_gas, initial_gas_and_reservoir,
    initial_message, intrinsic_gas, settle_gas, validate_block_gas_limit, validate_chain_id,
    validate_create_initcode, validate_floor_gas, validate_gas_price, validate_intrinsic_gas,
    validate_nonce_not_overflow, validate_priority_fee, validate_regular_gas_limit_cap,
    validate_sender, validate_tx_gas_limit_cap, warm_access_list, warm_base_accounts,
};
use crate::{
    EvmTypes, TxResult,
    env::TxEnv,
    evm::error_handler,
    interpreter::Host,
    registry::{HandlerResult, TxRequest},
};
use alloy_consensus::TxEip1559;
use alloy_primitives::U256;

/// Executes an EIP-1559 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(req: TxRequest<'_, '_, T, TxEip1559>) -> HandlerResult<TxResult<T>> {
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
        caller,
        tx.to,
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    );
    // EIP-2780: state-dependent gas is charged at the runtime gas phase, not the intrinsic phase.
    validate_intrinsic_gas(tx.gas_limit, intrinsic, 0)?;
    let floor_gas = floor_gas(
        req.host.version(),
        caller,
        tx.to,
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    );
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to);
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    charge_upfront(req.host, caller, effective_gas_cost)?;
    req.host.state.account(&caller, false).map_err(error_handler!(req.host))?.bump_nonce();

    let (gas_limit, reservoir) =
        initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, 0, 0);
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) = initial_message(
        req.host, caller, tx.nonce, tx.to, &tx.input, tx.value, gas_limit, reservoir,
    )?;
    // Failed execution has already been rolled back to the message's own checkpoint (and halt gas
    // zeroed) inside `execute_message`, so the result settles directly.
    let result = req.host.execute_message(&tx_env, bytecode, &mut message);
    settle_gas(req.host, caller, gas_price, tx.gas_limit, floor_gas, 0, 0, result)
}
