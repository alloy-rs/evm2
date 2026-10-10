//! Transaction handler extension points.

use crate::{
    Evm, EvmTypes, TxResult,
    ethereum::{
        LazyTxEip7702,
        eip7702::{self, AuthorizationResult},
    },
    interpreter::{GasTracker, MessageResult},
    registry::HandlerResult,
};
use alloy_primitives::{Address, U256};
use derive_where::derive_where;

/// Values produced by transaction execution and consumed during gas settlement.
#[derive_where(Debug)]
pub struct GasSettlement<T: EvmTypes> {
    /// Transaction sender.
    pub caller: Address,
    /// Effective gas price used for this transaction.
    pub gas_price: U256,
    /// Transaction gas limit.
    pub gas_limit: u64,
    /// Calldata floor gas.
    pub floor_gas: u64,
    /// State gas charged before execution.
    pub initial_state_gas: u64,
    /// State gas refunded by pre-execution processing.
    pub state_refund: u64,
    /// Result of executing the top-level message, carrying the settled transaction-level gas
    /// ([`settle_initial_frame_gas`](crate::ethereum::settle_initial_frame_gas)).
    pub result: MessageResult<T>,
}

/// Static extension points shared by transaction handlers.
pub trait TxHandlerHooks<T: EvmTypes>: Sized {
    /// Adjusts the intrinsic execution gas, state gas, and floor gas calculated by the standard
    /// handler before gas-limit validation. The adjusted floor gas is also used during settlement.
    fn adjust_intrinsic_gas(
        _host: &mut Evm<'_, T>,
        _envelope: &T::Tx,
        _intrinsic: &mut u64,
        _initial_state_gas: &mut u64,
        _floor_gas: &mut u64,
    ) -> HandlerResult<()> {
        Ok(())
    }

    /// Runs after standard pre-execution state changes and immediately before the execution
    /// checkpoint is created.
    fn before_execution(
        host: &mut Evm<'_, T>,
        _envelope: &T::Tx,
        caller: Address,
        upfront_fee: U256,
    ) -> HandlerResult<()> {
        crate::ethereum::charge_upfront(host, caller, upfront_fee)?;
        Ok(())
    }

    /// Validates and applies authorizations before the initial execution frame is created.
    ///
    /// The default preserves Ethereum's fork-dependent runtime charges and refunds. Custom
    /// implementations can reuse [`eip7702::apply_auth_list`] with their own accounting.
    /// The handler credits the returned refunds and rolls back delegations on authorization
    /// out-of-gas; hooks must not credit the returned refunds themselves.
    fn apply_authorizations(
        host: &mut Evm<'_, T>,
        _envelope: &T::Tx,
        tx: &LazyTxEip7702,
        caller: Address,
        gas: &mut GasTracker,
    ) -> HandlerResult<AuthorizationResult> {
        eip7702::apply_authorizations(host, tx, caller, gas)
    }

    /// Settles a transaction after execution and rollback handling.
    fn settle_transaction(
        host: &mut Evm<'_, T>,
        _envelope: &T::Tx,
        gas: GasSettlement<T>,
    ) -> HandlerResult<TxResult<T>> {
        crate::ethereum::default_settle_gas(host, gas)
    }
}

/// Hooks that preserve the default transaction behavior.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultTxHandlerHooks;

impl<T: EvmTypes> TxHandlerHooks<T> for DefaultTxHandlerHooks {}
