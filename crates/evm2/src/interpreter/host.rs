use super::{Gas, GasTracker, InstrStop, Message, Result, Word};
use crate::{
    BaseEvmTypes, DatabaseError, EvmTypesHost, ExecutionConfig, ExecutionError, HostError, SpecId,
    env::{BlockEnv, TxEnv},
    evm::{AccountLoad, SLoad, SStore, SelfDestructResult},
    utils::address_to_word,
};
use alloc::borrow::Cow;
use alloy_primitives::{Address, B256, Bytes};

/// Result of executing a call/create message for an EVM type family.
pub type MessageResult<T = BaseEvmTypes> = MessageResultExt<<T as EvmTypesHost>::MessageResultExt>;

/// Result of executing a call/create message, parameterized by extension data.
///
/// [`Self::gas`] is normalized for the frame's [`stop`](Self::stop) reason by the executor
/// before it is returned, so applying a child result to its caller is just
/// [`GasTracker::merge_child_gas`]. Use [`Self::gas_remaining_after_final_refund`] or
/// [`Self::gas_used_after_final_refund`] for top-level transaction accounting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageResultExt<E = ()> {
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Gas accounting for the child frame.
    pub gas: GasTracker,
    /// Return or revert output.
    pub output: Bytes,
    /// Created address for successful create messages.
    pub created_address: Option<Address>,
    /// EVM type-specific extension data.
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<E> MessageResultExt<E> {
    /// Replaces the extension, preserving all other message result fields.
    #[inline]
    pub fn with_ext<F>(self, ext: F) -> MessageResultExt<F> {
        self.map_ext(|_| ext)
    }

    /// Transforms the extension, preserving all other message result fields.
    #[inline]
    pub fn map_ext<F>(self, f: impl FnOnce(E) -> F) -> MessageResultExt<F> {
        let Self { stop, gas, output, created_address, ext, _non_exhaustive } = self;
        MessageResultExt { stop, gas, output, created_address, ext: f(ext), _non_exhaustive }
    }

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
            .map(|address| address_to_word(&address))
            .unwrap_or_default()
    }

    /// Calculates the final refund amount for a top-level transaction.
    #[inline]
    pub const fn final_refund(&self, gas_limit: u64, max_refund_quotient: u64) -> u64 {
        if self.gas.refunded() <= 0 {
            return 0;
        }
        // EIP-8037: the unused state-gas reservoir is reimbursed to the caller, so it
        // is not part of the gas actually spent. The EIP-3529 refund cap is a fraction
        // of the gas the transaction truly consumed, so the reservoir must be excluded
        // — otherwise a large idle reservoir inflates `spent` and disables the cap.
        let spent =
            gas_limit.saturating_sub(self.gas.remaining()).saturating_sub(self.gas.reservoir());
        let refund = self.gas.refunded() as u64;
        let cap = spent / max_refund_quotient;
        if refund < cap { refund } else { cap }
    }

    /// Returns top-level gas remaining after applying the final refund cap.
    #[inline]
    pub const fn gas_remaining_after_final_refund(
        &self,
        gas_limit: u64,
        max_refund_quotient: u64,
    ) -> u64 {
        let refunded = self.final_refund(gas_limit, max_refund_quotient);
        // EIP-8037: the unused reservoir (already settled to its frame-start value
        // on failure by `rollback_state_gas`) is also reimbursed to the caller.
        let remaining =
            self.gas.remaining().saturating_add(self.gas.reservoir()).saturating_add(refunded);
        if remaining < gas_limit { remaining } else { gas_limit }
    }

    /// Returns top-level gas used after applying the final refund cap.
    #[inline]
    pub const fn gas_used_after_final_refund(
        &self,
        gas_limit: u64,
        max_refund_quotient: u64,
    ) -> u64 {
        gas_limit
            .saturating_sub(self.gas_remaining_after_final_refund(gas_limit, max_refund_quotient))
    }
}

/// External host operations.
pub trait Host<T: EvmTypesHost> {
    /// Returns the active base specification ID.
    fn spec_id(&self) -> SpecId;

    /// Returns the active execution configuration.
    ///
    /// The interpreter keeps a reference to the returned version for the whole run, so the host
    /// must not replace or mutate it while the interpreter runs. An inspected run reads the config
    /// again when its inspector changes the opcodes it steps.
    fn execution_config(&self) -> &ExecutionConfig<T>;

    /// Returns the block environment.
    ///
    /// Compiled code keeps a pointer to the returned environment for the whole run, so the host
    /// must keep it at a stable address and unchanged while the interpreter runs.
    fn block_env(&mut self) -> &BlockEnv<T>;

    /// Returns the transaction environment.
    ///
    /// Compiled code keeps a pointer to the returned environment for the whole run, so the host
    /// must keep it at a stable address while the interpreter runs.
    fn tx_env(&mut self) -> &TxEnv<T>;

    /// Loads account information.
    fn load_account(
        &mut self,
        address: &Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, HostError>;

    /// Returns a historical block hash.
    fn block_hash(&mut self, number: &Word) -> Result<B256, DatabaseError>;

    /// Loads a persistent storage slot.
    fn sload(
        &mut self,
        address: &Address,
        key: &Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, HostError>;

    /// Stores a persistent storage slot.
    fn sstore(
        &mut self,
        address: &Address,
        key: &Word,
        value: &Word,
        skip_cold_load: bool,
    ) -> Result<SStore, HostError>;

    /// Loads a transient storage slot.
    fn tload(&mut self, address: &Address, key: &Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, address: &Address, key: &Word, value: &Word);

    /// Records a log emitted from `address`.
    ///
    /// `LOG0`-`LOG4` pass `topics` and `data` borrowed from interpreter buffers, and other callers
    /// may pass owned values. The host copies borrowed values only if it keeps the log.
    fn log(&mut self, address: &Address, topics: Cow<'_, [B256]>, data: Cow<'_, [u8]>);

    /// Executes a message inside this host.
    fn execute_message(
        &mut self,
        message: &mut Message<T>,
    ) -> Result<MessageResult<T>, ExecutionError>;

    /// Whether the active transaction uses independent frame budgets.
    fn frame_transaction_active(&self) -> bool {
        false
    }

    /// Reads a transaction parameter under the selected frame protocol.
    fn frame_tx_param(&self, _param: Word, _state_left: u64) -> Option<Word> {
        None
    }

    /// Reads one frame's immutable calldata.
    fn frame_data(&self, _index: Word) -> Option<Bytes> {
        None
    }

    /// Reads frame metadata or a completed frame's outcome.
    fn frame_param(&self, _index: Word, _param: Word) -> Option<Word> {
        None
    }

    /// Reads protocol signature metadata.
    fn frame_signature_param(&self, _index: Word, _param: Word) -> Option<Word> {
        None
    }

    /// Reads an arbitrary application signature witness.
    fn frame_signature_bytes(&self, _index: Word) -> Option<Bytes> {
        None
    }

    /// Whether the current top-level frame is a read-only assertion.
    fn is_assert_frame(&self) -> bool {
        false
    }

    /// Atomically applies approval and returns its state-gas charge.
    fn approve_frame(
        &mut self,
        _address: Address,
        _scope: Word,
        _state_left: u64,
    ) -> Result<u64, HostError> {
        Err(InstrStop::OpcodeNotFound.into())
    }

    /// Journals a storage charge/refill and returns the spendable refill for this frame.
    fn frame_sstore_gas(
        &mut self,
        _address: Address,
        _key: Word,
        _charge: u64,
        refill: u64,
    ) -> u64 {
        refill
    }

    /// Reads a recorded transaction trace value in ASSERT mode.
    fn txtrace(&mut self, _param: Word, _index: Word) -> Option<Word> {
        None
    }

    /// Reads one recorded event's data in ASSERT mode.
    fn event_data(&self, _index: Word) -> Option<Bytes> {
        None
    }

    /// Adopts another account's code, charging the opcode's dynamic gas.
    fn set_code_from(
        &mut self,
        _contract: &Address,
        _source: &Address,
        _gas: &mut Gas,
    ) -> Result<bool, HostError> {
        Err(InstrStop::OpcodeNotFound.into())
    }

    /// Transfers the current contract's balance and applies the active destruction rules.
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, HostError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::GasTracker;

    #[test]
    fn final_refund_uses_configured_quotient() {
        let mut result =
            MessageResult::<BaseEvmTypes> { gas: GasTracker::new(100), ..Default::default() };
        result.gas.set_remaining(20);
        result.gas.set_refunded(60);

        assert_eq!(result.final_refund(100, 2), 40);
        assert_eq!(result.final_refund(100, 5), 16);
        assert_eq!(result.final_refund(100, 1), 60);
    }
}
