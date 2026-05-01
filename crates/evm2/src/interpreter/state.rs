use super::{BytecodeRef, GasParams, InstrStop, Interpreter, Memory, Message, SpecId, Word};
use crate::{
    AccountLoad,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
};
use alloy_primitives::{B256, Bytes, Log};
use core::fmt;

/// Interpreter state passed to instructions.
pub struct State<'a> {
    /// Active bytecode.
    pub bytecode: BytecodeRef<'a>,
    /// Host implementation.
    pub host: &'a mut (dyn Host + 'a),
    /// Cached transaction-global environment.
    pub tx: &'a TxEnv,
    /// Active frame-local call/create message.
    pub message: &'a Message,
    /// Linear memory.
    pub memory: &'a mut Memory,
    /// Return data from the last call-like operation.
    pub return_data: &'a Bytes,
    /// Active spec identifier.
    pub spec: SpecId,
    /// Dynamic gas parameters for the active spec.
    pub gas_params: &'a GasParams,
    /// Whether state-changing opcodes are forbidden.
    pub is_static: bool,
    pub(crate) raw_interp: *mut Interpreter,
}

impl fmt::Debug for State<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("bytecode", &self.bytecode)
            .field("tx", &self.tx)
            .field("message", &self.message)
            .field("memory", &self.memory)
            .field("return_data", &self.return_data)
            .field("spec", &self.spec)
            .field("gas_params", &self.gas_params)
            .field("is_static", &self.is_static)
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
        address: Word,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop>;

    /// Returns a historical block hash.
    fn block_hash(&mut self, number: u64) -> Option<B256>;

    /// Loads a persistent storage slot.
    fn sload(&mut self, index: Word) -> Word;

    /// Stores a persistent storage slot.
    fn sstore(&mut self, index: Word, value: Word);

    /// Loads a transient storage slot.
    fn tload(&mut self, index: Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, index: Word, value: Word);

    /// Records an emitted log.
    fn log(&mut self, log: Log);

    /// Runs an interpreter frame inside this host.
    fn run_interpreter(
        &mut self,
        _tx_env: TxEnv,
        _bytecode: Bytecode,
        _message: Message,
    ) -> InstrStop {
        InstrStop::FatalExternalError
    }
}
