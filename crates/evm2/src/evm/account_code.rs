//! Account code representation and frame source selection.

use super::{AccountCodeInfo, AccountLoad};
use crate::bytecode::{Bytecode, CodeChunk};

impl AccountLoad {
    /// Selects the frame's code after gas and account resolution; the caller supplies
    /// the metered chunk load so top-level and nested calls share the same choice.
    pub(crate) fn frame_code<E>(
        &self,
        tip1143: bool,
        load_chunk: impl FnOnce() -> Result<CodeChunk, E>,
    ) -> Result<CodeChunk, E> {
        if !tip1143 {
            return Ok(self.code.clone().into());
        }
        if matches!(self.code_info, AccountCodeInfo::Empty) {
            Ok(Bytecode::default().into())
        } else {
            load_chunk()
        }
    }
}
