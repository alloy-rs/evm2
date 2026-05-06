use super::{BytecodeRef, Gas, InstrStop, Interpreter, Memory, Message, Word};
use crate::{
    EvmTypes, SpecId, Version,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    evm::{AccountLoad, SLoad, SStore, SelfDestructResult},
    version::GasParams,
};
use alloy_primitives::{Address, B256, Bytes, Log};
use core::fmt;

/// Interpreter state passed to instructions.
pub struct State<'a, T: EvmTypes> {
    /// Active bytecode.
    pub bytecode: BytecodeRef<'a>,
    /// Host implementation.
    pub host: &'a mut T::Host,
    /// Active spec identifier.
    pub spec: SpecId,
    pub(crate) tmp: InstrStop,
    /// Active runtime version data.
    pub version: &'a Version,
    pub(crate) raw_interp: *mut Interpreter<'a, T>,
}

impl<'a, T: EvmTypes> State<'a, T> {
    #[inline]
    const fn interp(&self) -> &Interpreter<'a, T> {
        // SAFETY: `raw_interp` is valid for the duration of instruction execution. Methods on
        // `State` must not borrow fields already passed separately to the instruction, such as
        // stack and gas.
        unsafe { &*self.raw_interp }
    }

    #[inline]
    fn interp_mut(&mut self) -> &mut Interpreter<'a, T> {
        // SAFETY: `raw_interp` is valid for the duration of instruction execution. Methods on
        // `State` must not borrow fields already passed separately to the instruction, such as
        // stack and gas.
        unsafe { &mut *self.raw_interp }
    }

    /// Returns interpreter gas.
    #[doc(hidden)]
    #[inline]
    pub fn gas(&mut self) -> &'a mut Gas {
        // SAFETY: `raw_interp` is valid for the duration of instruction execution.
        unsafe { &mut (*self.raw_interp).gas }
    }

    /// Returns the cached transaction-global environment.
    #[inline]
    pub(crate) const fn tx(&self) -> &TxEnv {
        self.interp().tx_env()
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub(crate) const fn message(&self) -> &Message {
        self.interp().message()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub(crate) const fn is_static(&self) -> bool {
        self.interp().is_static()
    }

    /// Returns the active dynamic gas parameters.
    #[inline]
    pub const fn gas_params(&self) -> &'a GasParams {
        &self.version.gas_params
    }

    /// Returns linear memory.
    #[inline]
    pub(crate) fn memory(&mut self) -> &mut Memory {
        self.interp_mut().memory()
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub(crate) const fn return_data(&self) -> &Bytes {
        self.interp().return_data()
    }

    /// Sets return data from the last call-like operation.
    #[inline]
    pub(crate) fn set_return_data(&mut self, return_data: Bytes) {
        self.interp_mut().return_data = return_data;
    }

    /// Sets the current frame output.
    #[inline]
    pub(crate) fn set_output(&mut self, output: *const [u8]) {
        self.interp_mut().set_output(output);
    }
}

impl<T: EvmTypes> fmt::Debug for State<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("bytecode", &self.bytecode)
            .field("tx", &self.tx())
            .field("message", &self.message())
            .field("is_static", &self.is_static())
            .field("memory", &self.interp().memory)
            .field("return_data", &self.return_data())
            .field("spec", &self.spec)
            .field("tmp", &self.tmp)
            .field("version", &self.version)
            .field("raw_interp", &self.raw_interp)
            .finish_non_exhaustive()
    }
}

/// Result of executing a call/create message.
///
/// Gas accounting is split into unused gas and the refund counter because EVM refunds are not
/// immediately spendable by the parent frame and are capped only at the top-level transaction. Use
/// [`Self::gas_returned_to_parent`] and [`Self::refund_propagated_to_parent`] when applying a child
/// result to a caller frame. Use [`Self::gas_remaining_after_final_refund`] or
/// [`Self::gas_used_after_final_refund`] for top-level transaction accounting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageResult {
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Gas left in the child frame before refund accounting.
    ///
    /// Do not use this field directly for parent-frame or transaction-level refund handling; see
    /// the [`MessageResult`] docs and helper methods.
    pub gas_remaining: u64,
    /// Raw refund counter produced by this frame.
    ///
    /// This value may be negative locally. Do not use this field directly for propagation or final
    /// transaction refunds; see the [`MessageResult`] docs and helper methods.
    pub gas_refunded: i64,
    /// Return or revert output.
    pub output: Bytes,
    /// Created address for successful create messages.
    pub created_address: Option<Address>,
}

impl MessageResult {
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
        if self.returns_unused_gas() { self.gas_remaining } else { 0 }
    }

    /// Returns the refund counter delta that should be propagated to the parent frame.
    #[inline]
    pub const fn refund_propagated_to_parent(&self) -> i64 {
        if self.stop.is_success() { self.gas_refunded } else { 0 }
    }

    /// Calculates the final refund amount for a top-level transaction.
    #[inline]
    pub const fn final_refund(&self, gas_limit: u64, is_london: bool) -> u64 {
        if self.gas_refunded <= 0 {
            return 0;
        }
        let max_refund_quotient = if is_london { 5 } else { 2 };
        let spent = gas_limit.saturating_sub(self.gas_remaining);
        let refund = self.gas_refunded as u64;
        let cap = spent / max_refund_quotient;
        if refund < cap { refund } else { cap }
    }

    /// Returns top-level gas remaining after applying the final refund cap.
    #[inline]
    pub const fn gas_remaining_after_final_refund(&self, gas_limit: u64, is_london: bool) -> u64 {
        let refunded = self.final_refund(gas_limit, is_london);
        let remaining = self.gas_remaining.saturating_add(refunded);
        if remaining < gas_limit { remaining } else { gas_limit }
    }

    /// Returns top-level gas used after applying the final refund cap.
    #[inline]
    pub const fn gas_used_after_final_refund(&self, gas_limit: u64, is_london: bool) -> u64 {
        gas_limit.saturating_sub(self.gas_remaining_after_final_refund(gas_limit, is_london))
    }
}

/// External host operations.
pub trait Host {
    /// Returns the active base specification ID.
    fn spec_id(&self) -> SpecId;

    /// Returns the block environment.
    fn block_env(&mut self) -> &BlockEnv;

    /// Loads account information.
    fn load_account(
        &mut self,
        address: Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop>;

    /// Returns whether an account is empty/non-existent for new-account gas checks.
    fn target_is_empty_for_new_account_gas(&mut self, address: Address, spec: SpecId) -> bool;

    /// Returns a historical block hash.
    fn block_hash(&mut self, number: Word) -> Option<B256>;

    /// Loads a persistent storage slot.
    fn sload(
        &mut self,
        address: Address,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop>;

    /// Stores a persistent storage slot.
    fn sstore(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop>;

    /// Loads a transient storage slot.
    fn tload(&mut self, address: Address, key: Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, address: Address, key: Word, value: Word);

    /// Records an emitted log.
    fn log(&mut self, log: Log);

    /// Executes a message inside this host.
    fn execute_message(
        &mut self,
        tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> MessageResult;

    /// Registers the current contract for self-destruction.
    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop>;
}
