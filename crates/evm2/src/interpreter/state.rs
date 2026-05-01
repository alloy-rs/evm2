use super::{BytecodeRef, Interpreter, Memory, SpecId, Word};
use core::fmt;

/// Interpreter state passed to instructions.
#[allow(unused)]
pub struct State<'a> {
    /// Active bytecode.
    pub bytecode: BytecodeRef<'a>,
    /// Host implementation.
    pub host: &'a mut (dyn Host + 'a),
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
            .field("memory", &self.memory)
            .field("spec", &self.spec)
            .field("raw_interp", &self.raw_interp)
            .finish_non_exhaustive()
    }
}

/// External host operations.
pub trait Host {
    /// Returns an account balance.
    fn balance(&self, address: Word) -> Word;
}
