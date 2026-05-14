//! Opcount tracing inspector that simply counts all opcodes.

use evm2::{EvmTypes, Inspector, interpreter::Interpreter};

/// An inspector that counts all opcodes.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpcodeCountInspector {
    /// Opcode counter.
    count: usize,
}

impl OpcodeCountInspector {
    /// Returns the opcode counter.
    #[inline]
    pub const fn count(&self) -> usize {
        self.count
    }
}

impl<T: EvmTypes> Inspector<T> for OpcodeCountInspector {
    fn step(&mut self, _interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        self.count += 1;
    }
}
