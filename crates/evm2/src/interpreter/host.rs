use super::{GasTracker, InstrStop, Message, Result, Word};
use crate::{
    BaseEvmTypes, EvmTypes, SpecId,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    evm::{AccountLoad, SLoad, SStore, SelfDestructResult},
};
use alloy_primitives::{Address, B256, Bytes, Log};
use derive_where::derive_where;

/// Result of executing a call/create message.
///
/// Gas accounting is split into unused gas and the refund counter because EVM refunds are not
/// immediately spendable by the parent frame and are capped only at the top-level transaction. Use
/// [`Self::gas_returned_to_parent`] and [`Self::refund_propagated_to_parent`] when applying a child
/// result to a caller frame. Use [`Self::gas_remaining_after_final_refund`] or
/// [`Self::gas_used_after_final_refund`] for top-level transaction accounting.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::MessageResultExt)]
pub struct MessageResult<T: EvmTypes = BaseEvmTypes> {
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Gas accounting for the child frame.
    pub gas: GasTracker,
    /// Return or revert output.
    pub output: Bytes,
    /// Created address for successful create messages.
    pub created_address: Option<Address>,
    /// EVM type-specific extension data.
    pub ext: T::MessageResultExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypes> MessageResult<T> {
    /// Returns whether the message committed state changes.
    #[inline]
    pub const fn is_success(&self) -> bool {
        self.stop.is_success()
    }

    /// Returns whether the message can return unused gas to its parent frame.
    #[inline]
    pub const fn returns_unused_gas(&self) -> bool {
        self.stop.is_success() || self.stop.is_revert()
    }

    /// Returns unused gas that should be returned to the parent frame.
    #[inline]
    pub const fn gas_returned_to_parent(&self) -> u64 {
        if self.returns_unused_gas() { self.gas.remaining() } else { 0 }
    }

    /// Returns the refund counter delta that should be propagated to the parent frame.
    #[inline]
    pub const fn refund_propagated_to_parent(&self) -> i64 {
        if self.stop.is_success() { self.gas.refunded() } else { 0 }
    }

    /// Calculates the final refund amount for a top-level transaction.
    #[inline]
    pub const fn final_refund(&self, gas_limit: u64, is_london: bool) -> u64 {
        if self.gas.refunded() <= 0 {
            return 0;
        }
        let max_refund_quotient = if is_london { 5 } else { 2 };
        let spent = gas_limit.saturating_sub(self.gas.remaining());
        let refund = self.gas.refunded() as u64;
        let cap = spent / max_refund_quotient;
        if refund < cap { refund } else { cap }
    }

    /// Returns top-level gas remaining after applying the final refund cap.
    #[inline]
    pub const fn gas_remaining_after_final_refund(&self, gas_limit: u64, is_london: bool) -> u64 {
        let refunded = self.final_refund(gas_limit, is_london);
        let remaining = self.gas.remaining().saturating_add(refunded);
        if remaining < gas_limit { remaining } else { gas_limit }
    }

    /// Returns top-level gas used after applying the final refund cap.
    #[inline]
    pub const fn gas_used_after_final_refund(&self, gas_limit: u64, is_london: bool) -> u64 {
        gas_limit.saturating_sub(self.gas_remaining_after_final_refund(gas_limit, is_london))
    }
}

/// External host operations.
pub trait Host<T: EvmTypes> {
    /// Returns the active base specification ID.
    fn spec_id(&self) -> SpecId;

    /// Returns the block environment.
    fn block_env(&mut self) -> &BlockEnv<T>;

    /// Loads account information.
    fn load_account(
        &mut self,
        address: &Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop>;

    /// Returns whether an account is empty/non-existent for new-account gas checks.
    fn target_is_empty_for_new_account_gas(
        &mut self,
        address: &Address,
        spec: SpecId,
    ) -> Result<bool, InstrStop>;

    /// Returns a historical block hash.
    fn block_hash(&mut self, number: &Word) -> Result<Option<B256>, InstrStop>;

    /// Loads a persistent storage slot.
    fn sload(
        &mut self,
        address: &Address,
        key: &Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop>;

    /// Stores a persistent storage slot.
    fn sstore(
        &mut self,
        address: &Address,
        key: &Word,
        value: &Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop>;

    /// Loads a transient storage slot.
    fn tload(&mut self, address: &Address, key: &Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, address: &Address, key: &Word, value: &Word);

    /// Records an emitted log.
    fn log(&mut self, log: Log);

    /// Executes a message inside this host.
    fn execute_message(
        &mut self,
        tx_env: &TxEnv<T>,
        bytecode: Bytecode,
        message: &mut Message<T>,
        caller_is_static: bool,
    ) -> MessageResult<T>;

    /// Derives the destination address for a create message before it is inspected.
    fn created_address(
        &mut self,
        bytecode: &Bytecode,
        message: &Message<T>,
    ) -> Result<Address, InstrStop>;

    /// Registers the current contract for self-destruction.
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop>;
}
