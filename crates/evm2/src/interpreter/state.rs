use super::{Interpreter, Memory, SpecId, Word};

/// Catch all. Rest of stuff, cold.
#[allow(unused)]
pub struct State<'a> {
    pub host: &'a mut (dyn Host + 'a),
    pub memory: &'a mut Memory,
    pub spec: SpecId,
    pub(crate) raw_interp: *mut Interpreter,
}

pub trait Host {
    fn balance(&self, address: Word) -> Word;
}
