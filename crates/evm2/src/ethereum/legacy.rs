use super::{
    charge_upfront, create_initial_state_gas, floor_gas, initial_gas_and_reservoir,
    initial_message, intrinsic_gas, refund_create_state_gas, rollback_failed_execution, settle_gas,
    validate_block_gas_limit, validate_chain_id, validate_create_initcode, validate_floor_gas,
    validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow,
    validate_regular_gas_limit_cap, validate_sender, validate_tx_gas_limit_cap, warm_base_accounts,
};
use crate::{
    EvmTypes, TxResult,
    env::TxEnv,
    evm::error_handler,
    interpreter::Host,
    registry::{HandlerResult, TxRequest},
};
use alloy_consensus::TxLegacy;
use alloy_primitives::U256;

/// Executes a legacy transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(req: TxRequest<'_, '_, T, TxLegacy>) -> HandlerResult<TxResult<T>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let gas_price = U256::from(tx.gas_price);

    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), tx.chain_id, true)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to, &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let intrinsic = intrinsic_gas(req.host.version(), caller, tx.to, &tx.input, 0, 0, tx.value);
    let create_state_gas = create_initial_state_gas(req.host.version(), tx.to.is_create());
    let (initial_state_gas, first_frame_state_gas) =
        if req.host.feature(crate::EvmFeatures::EIP2780) {
            (0, create_state_gas)
        } else {
            (create_state_gas, 0)
        };
    validate_intrinsic_gas(tx.gas_limit, intrinsic, initial_state_gas)?;
    let floor_gas = floor_gas(req.host.version(), caller, tx.to, tx.value, &tx.input, 0, 0);
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_regular_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * gas_price;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to);

    charge_upfront(req.host, caller, max_gas_cost)?;
    req.host.state.account(&caller, false).map_err(error_handler!(req.host))?.bump_nonce();
    let execution_checkpoint = req.host.state.checkpoint();

    let (gas_limit, reservoir) = initial_gas_and_reservoir(
        req.host.version(),
        tx.gas_limit,
        intrinsic,
        initial_state_gas,
        0,
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
        tx.to,
        &tx.input,
        tx.value,
        gas_limit,
        reservoir,
        first_frame_state_gas,
    )?;
    let mut result = req.host.execute_message(&tx_env, bytecode, &mut message);
    rollback_failed_execution(req.host, execution_checkpoint, &mut result);
    refund_create_state_gas(&mut result, initial_state_gas);

    settle_gas(
        req.host,
        caller,
        gas_price,
        tx.gas_limit,
        floor_gas,
        initial_state_gas,
        0,
        tx.to.is_create(),
        result,
    )
}
