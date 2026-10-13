//! Account code representation and frame source selection.

use super::AccountLoad;
use crate::{
    EvmFeatures,
    bytecode::{Bytecode, CodeChunk},
    interpreter::CodeSource,
};

/// How the active version obtains account runtime code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodeAccessMode {
    Eager,
    MeteredChunks,
}

impl CodeAccessMode {
    pub(crate) const fn from_features(features: EvmFeatures) -> Self {
        if features.contains(EvmFeatures::TIP1143) { Self::MeteredChunks } else { Self::Eager }
    }

    pub(crate) const fn loads_full_code(self) -> bool {
        matches!(self, Self::Eager)
    }
}

/// The account's code representation, separate from how its bytes are loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountCodeKind {
    Empty,
    Legacy,
    Chunked { size: u32 },
}

impl AccountLoad {
    pub(crate) fn code_kind(&self, mode: CodeAccessMode) -> AccountCodeKind {
        match mode {
            CodeAccessMode::Eager => {
                if self.code.is_empty() {
                    AccountCodeKind::Empty
                } else {
                    AccountCodeKind::Legacy
                }
            }
            CodeAccessMode::MeteredChunks => {
                if self.code_size == Some(0) {
                    AccountCodeKind::Empty
                } else if self.is_chunked {
                    AccountCodeKind::Chunked { size: self.code_size.expect("chunked code size") }
                } else {
                    AccountCodeKind::Legacy
                }
            }
        }
    }

    /// Selects the frame's code after gas and account resolution; the caller supplies
    /// the metered chunk load so top-level and nested calls share the same choice.
    pub(crate) fn code_source<E>(
        &self,
        mode: CodeAccessMode,
        load_chunk: impl FnOnce() -> Result<CodeChunk, E>,
    ) -> Result<CodeSource, E> {
        if mode.loads_full_code() {
            return Ok(self.code.clone().into());
        }
        match self.code_kind(mode) {
            AccountCodeKind::Empty => Ok(Bytecode::default().into()),
            AccountCodeKind::Legacy | AccountCodeKind::Chunked { .. } => {
                Ok(CodeSource::Account(load_chunk()?))
            }
        }
    }
}
