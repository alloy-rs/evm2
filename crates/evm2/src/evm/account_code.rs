//! Account code representation and frame source selection.

use super::AccountLoad;
use crate::{
    EvmFeatures,
    bytecode::{Bytecode, CodeChunk},
    interpreter::CodeSource,
};
use alloy_primitives::Address;

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

/// The account's code identity, separate from how its bytes are loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountCodeKind {
    Empty,
    Legacy,
    Delegated(Address),
    Chunked { size: u32 },
}

impl AccountCodeKind {
    pub(crate) const fn delegation_target(self) -> Option<Address> {
        if let Self::Delegated(target) = self { Some(target) } else { None }
    }
}

impl AccountLoad {
    pub(crate) fn code_kind(&self, mode: CodeAccessMode) -> AccountCodeKind {
        match mode {
            CodeAccessMode::Eager => {
                if self.code.is_empty() {
                    AccountCodeKind::Empty
                } else if let Some(target) = self.code.eip7702_address() {
                    AccountCodeKind::Delegated(target)
                } else {
                    AccountCodeKind::Legacy
                }
            }
            CodeAccessMode::MeteredChunks => {
                if let Some(target) = self.inline_delegation {
                    AccountCodeKind::Delegated(target)
                } else if self.code_size == Some(0) {
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
            AccountCodeKind::Delegated(target) => Ok(Bytecode::new_eip7702(target).into()),
            AccountCodeKind::Legacy | AccountCodeKind::Chunked { .. } => {
                Ok(CodeSource::Account(load_chunk()?))
            }
        }
    }
}
