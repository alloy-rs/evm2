use super::{Interpreter, Memory, SpecId, Word};
use core::fmt;

/// Catch all. Rest of stuff, cold.
#[allow(unused)]
pub struct State<'a> {
    pub host: &'a mut (dyn Host + 'a),
    pub memory: &'a mut Memory,
    pub spec: SpecId,
    pub(crate) raw_interp: *mut Interpreter,
}

impl fmt::Debug for State<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("memory", &self.memory)
            .field("spec", &self.spec)
            .field("raw_interp", &self.raw_interp)
            .finish_non_exhaustive()
    }
}

pub trait Host {
    fn balance(&self, address: Word) -> Word;
}
