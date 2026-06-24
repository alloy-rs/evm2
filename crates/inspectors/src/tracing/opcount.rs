//! Opcount tracing inspector that simply counts all opcodes.
//!
//! See also <https://geth.ethereum.org/docs/developers/evm-tracing/built-in-tracers>

use evm2::{EvmTypes, Inspector, interpreter::Interpreter};

/// An inspector that counts all opcodes.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpcodeCountInspector {
    /// Opcode counter.
    count: usize,
}

impl OpcodeCountInspector {
    /// Returns the opcode counter
    #[inline]
    pub const fn count(&self) -> usize {
        self.count
    }
}

impl<T: EvmTypes> Inspector<T> for OpcodeCountInspector {
    fn step(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
        self.count += 1;
    }
}
