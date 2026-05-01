use super::{BytecodeRef, Interpreter, Memory, SpecId, Word};
use crate::env::{BlockEnv, TxEnv};
use alloy_primitives::B256;
use core::fmt;

/// Interpreter state passed to instructions.
pub struct State<'a> {
    /// Active bytecode.
    pub bytecode: BytecodeRef<'a>,
    /// Host implementation.
    pub host: &'a (dyn Host + 'a),
    /// Cached transaction environment.
    pub tx: &'a TxEnv,
    /// Cached block environment.
    pub block: &'a BlockEnv,
    /// Linear memory.
    pub memory: &'a mut Memory,
    /// Active spec identifier.
    pub spec: SpecId,
    pub(crate) raw_interp: *mut Interpreter,
}

impl fmt::Debug for State<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("bytecode", &self.bytecode)
            .field("tx", &self.tx)
            .field("block", &self.block)
            .field("memory", &self.memory)
            .field("spec", &self.spec)
            .field("raw_interp", &self.raw_interp)
            .finish_non_exhaustive()
    }
}

/// External host operations.
pub trait Host {
    /// Returns the transaction environment.
    fn tx_env(&self) -> &TxEnv;

    /// Returns the block environment.
    fn block_env(&self) -> &BlockEnv;

    /// Returns an account balance.
    fn balance(&self, address: Word) -> Word;

    /// Returns an account's code size.
    fn get_code_size(&self, address: Word) -> usize;

    /// Returns an account's code hash.
    fn get_code_hash(&self, address: Word) -> B256;

    /// Returns a historical block hash.
    fn block_hash(&self, number: u64) -> Option<B256>;
}
