use super::{
    charge_upfront, floor_gas, initial_message, intrinsic_gas, rollback_failed_execution,
    settle_gas, validate_block_gas_limit, validate_chain_id, validate_create_initcode,
    validate_floor_gas, validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow,
    validate_regular_gas_limit_cap, validate_sender, validate_tx_gas_limit_cap, warm_base_accounts,
};
use crate::{
    Evm, EvmTypes, TxResult,
    env::TxEnv,
    interpreter::Host,
    registry::{HandlerResult, TxRequest},
};
use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::U256;

pub(super) fn handle<T: EvmTypes<Host = Evm<T>>>(
    req: TxRequest<'_, T, Recovered<TxLegacy>>,
) -> HandlerResult<TxResult<T>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let gas_price = U256::from(tx.gas_price);

    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), tx.chain_id, true)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to, &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let intrinsic = intrinsic_gas(req.host.version(), tx.to, &tx.input, 0, 0);
    validate_intrinsic_gas(tx.gas_limit, intrinsic)?;
    let floor_gas = floor_gas(req.host.version(), &tx.input, 0, 0);
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * gas_price;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to);

    charge_upfront(req.host, caller, max_gas_cost)?;
    match req.host.state.account_entry(&caller, false) {
        Ok(mut account) => {
            account.bump_nonce();
        }
        Err(code) => return Err(req.host.db_error_handler(code)),
    }
    let execution_checkpoint = req.host.state.checkpoint();

    let gas_limit = tx.gas_limit - intrinsic;
    let tx_env = TxEnv {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnv::default()
    };
    let (bytecode, mut message) =
        initial_message(req.host, caller, tx.nonce, tx.to, &tx.input, tx.value, gas_limit)?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message, false);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);

    settle_gas(req.host, caller, gas_price, tx.gas_limit, floor_gas, result)
}
