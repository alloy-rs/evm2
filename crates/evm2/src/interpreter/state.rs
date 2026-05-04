use super::{BytecodeRef, InstrStop, Interpreter, Memory, Message, SpecId, Word};
use crate::{
    AccountLoad, EvmTypes, SelfDestructResult, StorageLoad,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
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
    pub(crate) raw_interp: *mut Interpreter<T>,
}

impl<T: EvmTypes> State<'_, T> {
    #[inline]
    fn interp(&self) -> &Interpreter<T> {
        // SAFETY: `raw_interp` is valid for the duration of instruction execution. Methods on
        // `State` must not borrow fields already passed separately to the instruction, such as
        // stack and gas.
        unsafe { &*self.raw_interp }
    }

    #[inline]
    fn interp_mut(&mut self) -> &mut Interpreter<T> {
        // SAFETY: `raw_interp` is valid for the duration of instruction execution. Methods on
        // `State` must not borrow fields already passed separately to the instruction, such as
        // stack and gas.
        unsafe { &mut *self.raw_interp }
    }

    /// Returns the cached transaction-global environment.
    #[inline]
    pub(crate) fn tx(&self) -> &TxEnv {
        self.interp().tx_env()
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub(crate) fn message(&self) -> &Message {
        self.interp().message()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub(crate) fn is_static(&self) -> bool {
        self.interp().is_static()
    }

    /// Returns linear memory.
    #[inline]
    pub(crate) fn memory(&mut self) -> &mut Memory {
        self.interp_mut().memory()
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub(crate) fn return_data(&self) -> &Bytes {
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
            .field("raw_interp", &self.raw_interp)
            .finish_non_exhaustive()
    }
}

/// Result of executing a call/create message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageResult {
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Gas left in the child frame after refunds.
    pub gas_remaining: u64,
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
}

/// External host operations.
pub trait Host {
    /// Returns the active hard fork specification.
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

    /// Returns a historical block hash.
    fn block_hash(&mut self, number: u64) -> Option<B256>;

    /// Loads a persistent storage slot.
    fn sload(&mut self, address: Address, key: Word) -> StorageLoad;

    /// Stores a persistent storage slot.
    fn sstore(&mut self, address: Address, key: Word, value: Word);

    /// Loads a transient storage slot.
    fn tload(&mut self, address: Address, key: Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, address: Address, key: Word, value: Word);

    /// Records an emitted log.
    fn log(&mut self, log: Log);

    /// Executes a message inside this host.
    fn execute_message(
        &mut self,
        tx_env: TxEnv,
        bytecode: Bytecode,
        message: Message,
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
