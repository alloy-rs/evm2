//! Account code representation and frame source selection.

use super::AccountLoad;
use crate::{
    bytecode::{Bytecode, CodeChunk},
    interpreter::CodeSource,
};

/// The account's code representation, separate from how its bytes are loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountCodeKind {
    Empty,
    Legacy,
    Chunked { size: u32 },
}

impl AccountLoad {
    pub(crate) fn code_kind(&self) -> AccountCodeKind {
        if self.code_size == Some(0) {
            AccountCodeKind::Empty
        } else if self.is_chunked {
            AccountCodeKind::Chunked { size: self.code_size.expect("chunked code size") }
        } else {
            AccountCodeKind::Legacy
        }
    }

    /// Selects the frame's code after gas and account resolution; the caller supplies
    /// the metered chunk load so top-level and nested calls share the same choice.
    pub(crate) fn code_source<E>(
        &self,
        tip1143: bool,
        load_chunk: impl FnOnce() -> Result<CodeChunk, E>,
    ) -> Result<CodeSource, E> {
        if !tip1143 {
            return Ok(self.code.clone().into());
        }
        match self.code_kind() {
            AccountCodeKind::Empty => Ok(Bytecode::default().into()),
            AccountCodeKind::Legacy | AccountCodeKind::Chunked { .. } => {
                Ok(CodeSource::Account(load_chunk()?))
            }
        }
    }
}
