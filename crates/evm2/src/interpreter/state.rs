use super::{BytecodeRef, InstrStop, Interpreter, Memory, Message, SpecId, Word};
use crate::{
    AccountLoad, SelfDestructResult,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
};
use alloy_primitives::{Address, B256, Bytes, Log};
use core::fmt;

/// Interpreter state passed to instructions.
pub struct State<'a, H: Host + ?Sized> {
    /// Active bytecode.
    pub bytecode: BytecodeRef<'a>,
    /// Host implementation.
    pub host: &'a mut H,
    /// Active spec identifier.
    pub spec: SpecId,
    pub(crate) raw_interp: *mut Interpreter,
}

impl<H: Host + ?Sized> State<'_, H> {
    #[inline]
    fn interp(&self) -> &Interpreter {
        // SAFETY: `raw_interp` is valid for the duration of instruction execution. Methods on
        // `State` must not borrow fields already passed separately to the instruction, such as
        // stack and gas.
        unsafe { &*self.raw_interp }
    }

    #[inline]
    fn interp_mut(&mut self) -> &mut Interpreter {
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

    /// Sets the current frame output.
    #[inline]
    pub(crate) fn set_output(&mut self, output: *const [u8]) {
        self.interp_mut().set_output(output);
    }
}

impl<H: Host + ?Sized> fmt::Debug for State<'_, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("bytecode", &self.bytecode)
            .field("tx", &self.tx())
            .field("message", &self.message())
            .field("memory", &self.interp().memory)
            .field("return_data", &self.return_data())
            .field("spec", &self.spec)
            .field("raw_interp", &self.raw_interp)
            .finish_non_exhaustive()
    }
}

/// External host operations.
pub trait Host {
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
    fn sload(&mut self, address: Address, index: Word) -> Word;

    /// Stores a persistent storage slot.
    fn sstore(&mut self, address: Address, index: Word, value: Word);

    /// Loads a transient storage slot.
    fn tload(&mut self, address: Address, index: Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, address: Address, index: Word, value: Word);

    /// Records an emitted log.
    fn log(&mut self, log: Log);

    /// Executes a message inside this host.
    fn execute_message(
        &mut self,
        _tx_env: TxEnv,
        _bytecode: Bytecode,
        _message: Message,
    ) -> Result<Word, InstrStop> {
        Err(InstrStop::FatalExternalError)
    }

    /// Registers the current contract for self-destruction.
    fn selfdestruct(
        &mut self,
        _contract: Address,
        _target: Address,
        _skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        Err(InstrStop::FatalExternalError)
    }
}
