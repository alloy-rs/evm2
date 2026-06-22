use super::{GasTracker, InstrStop, Message, Result, Word};
use crate::{
    BaseEvmTypes, EvmFeatures, EvmTypes, SpecId,
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

    /// Returns the created address to push onto the parent frame's stack.
    ///
    /// Yields the created address as a stack word on success, or zero when the
    /// create failed (revert, halt, or an early-fail path that left no address).
    #[inline]
    pub fn created_address_for_parent(&self) -> Word {
        self.created_address
            .filter(|_| self.stop.is_success())
            .map(|address| Word::from_be_slice(address.as_slice()))
            .unwrap_or_default()
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

    /// Returns the EIP-8037 state gas this returning child frame accumulates into
    /// its parent's `state_gas_spent`.
    ///
    /// On success the parent absorbs the child's net state gas, which can be
    /// negative when 0→x→0 restorations outnumber 0→x creations (the negative
    /// contribution flows the parent's matching charge back out). On revert/halt
    /// the child's state changes roll back, so it contributes nothing. Always zero
    /// without EIP-8037.
    #[inline]
    pub const fn state_gas_to_parent(&self) -> i64 {
        if self.stop.is_success() { self.gas.state_gas_spent() } else { 0 }
    }

    /// Returns the EIP-8037 spilled state gas (`state_gas_from_gas_left`) this
    /// returning child frame merges into its parent.
    ///
    /// On success the spilled state gas the child funded from regular gas persists
    /// and is now backed by the parent's merged regular gas, so the parent absorbs
    /// it (a later parent rollback then returns it). On revert/halt the spilled
    /// gas has already been refilled by [`GasTracker::unwind_state_gas`], so the
    /// child contributes nothing. Always zero without EIP-8037.
    #[inline]
    pub const fn spilled_to_parent(&self) -> u64 {
        if self.stop.is_success() { self.gas.state_gas_spilled() } else { 0 }
    }

    /// Returns the EIP-8037 state-gas reservoir this returning child frame hands
    /// back to its parent.
    ///
    /// The reservoir is a shared pool the child inherited from the parent at call
    /// time (see [`Message::reservoir`](crate::interpreter::Message::reservoir)).
    /// On success the parent takes the child's final reservoir. On revert/halt the
    /// child's gas has already been rolled back by
    /// [`GasTracker::unwind_state_gas`], which restored the reservoir to the value
    /// the child inherited — so the parent's reservoir is left untouched in both
    /// cases. Always zero without EIP-8037.
    #[inline]
    pub const fn reservoir_to_parent(&self) -> u64 {
        self.gas.reservoir()
    }

    /// Calculates the final refund amount for a top-level transaction.
    #[inline]
    pub const fn final_refund(&self, gas_limit: u64, is_eip3529: bool) -> u64 {
        if self.gas.refunded() <= 0 {
            return 0;
        }
        let max_refund_quotient = if is_eip3529 { 5 } else { 2 };
        let spent = gas_limit.saturating_sub(self.gas.remaining());
        let refund = self.gas.refunded() as u64;
        let cap = spent / max_refund_quotient;
        if refund < cap { refund } else { cap }
    }

    /// Returns the leftover EIP-8037 state-gas reservoir reimbursed to the caller
    /// at the top level.
    ///
    /// On success this is the frame's final reservoir. On revert or halt
    /// [`GasTracker::unwind_state_gas`] has already restored the reservoir to its
    /// frame-start value: a regular-gas halt does not consume the separate
    /// state-gas reservoir, and any state gas that spilled into regular gas was
    /// returned to `remaining` (and so is consumed on halt, not reimbursed via the
    /// reservoir). Always zero without EIP-8037.
    #[inline]
    pub const fn reservoir_reimbursed(&self) -> u64 {
        self.gas.reservoir()
    }

    /// Returns top-level gas remaining after applying the final refund cap.
    #[inline]
    pub const fn gas_remaining_after_final_refund(&self, gas_limit: u64, is_eip3529: bool) -> u64 {
        let refunded = self.final_refund(gas_limit, is_eip3529);
        // EIP-8037: the unused reservoir is also reimbursed to the caller.
        let remaining = self
            .gas
            .remaining()
            .saturating_add(self.reservoir_reimbursed())
            .saturating_add(refunded);
        if remaining < gas_limit { remaining } else { gas_limit }
    }

    /// Returns top-level gas used after applying the final refund cap.
    #[inline]
    pub const fn gas_used_after_final_refund(&self, gas_limit: u64, is_eip3529: bool) -> u64 {
        gas_limit.saturating_sub(self.gas_remaining_after_final_refund(gas_limit, is_eip3529))
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
        features: EvmFeatures,
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

    /// Registers the current contract for self-destruction.
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop>;
}
