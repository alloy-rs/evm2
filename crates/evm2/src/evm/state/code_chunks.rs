//! Requested runtime analyses and checkpoint-local code warmth.

use super::{Account, AccountInfo, JournalEntry, State, StateInner};
use crate::{
    CodeChunkError, DatabaseError, LoadError,
    bytecode::{CodeChunk, LEGACY_CODE_CHUNK_SIZE, code_chunk},
    evm::db::{DbResult, DynDatabase},
};
use alloy_primitives::{Address, KECCAK256_EMPTY, map::hash_map::Entry};
use derive_where::derive_where;

/// One requested chunk, scoped to the account's current code identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountCodeChunk {
    /// Original payload, execution layout, and cached analysis.
    pub chunk: CodeChunk,
    /// Transaction-local logical warmth, independent of byte residency.
    pub is_warm: bool,
}

/// Successful payload load and logical warmth before the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeChunkLoad {
    /// Original payload, retaining a known legacy/delegation kind.
    pub chunk: CodeChunk,
    /// Whether this request changed the account chunk from cold to warm.
    pub is_cold: bool,
}

/// A loaded code chunk with journaled access warmth, like a storage slot handle.
#[derive_where(Debug)]
pub struct CodeChunkHandle<'a, 'db> {
    address: Address,
    index: u32,
    chunk: &'a mut AccountCodeChunk,
    #[derive_where(skip)]
    inner: &'a mut StateInner<'db>,
}

impl<'a, 'db> CodeChunkHandle<'a, 'db> {
    fn load(
        address: Address,
        index: u32,
        account: &'a mut Account,
        inner: &'a mut StateInner<'db>,
        skip_cold_load: bool,
    ) -> Result<Option<Self>, LoadError> {
        let Some(info) = &account.present else { return Ok(None) };
        if info.code_hash.is_zero() || info.code_hash == KECCAK256_EMPTY {
            return Ok(None);
        }
        let expected = if let Some(metadata) = &info.code_metadata() {
            let Some(length) = metadata.chunk_len(index) else { return Ok(None) };
            Some(length)
        } else {
            if index != 0 {
                return Ok(None);
            }
            None
        };
        let chunk = match account.code_chunks.entry(index) {
            Entry::Occupied(entry) => {
                if skip_cold_load && !entry.get().is_warm {
                    return Err(LoadError::ColdLoadSkipped);
                }
                entry.into_mut()
            }
            Entry::Vacant(entry) => {
                if skip_cold_load {
                    return Err(LoadError::ColdLoadSkipped);
                }
                let chunk = load_chunk(inner, info, index, expected)?;
                if expected.is_none() {
                    account.code_size = Some(chunk.original_bytes().len() as u32);
                }
                entry.insert(AccountCodeChunk { chunk, is_warm: false })
            }
        };
        Ok(Some(Self { address, index, chunk, inner }))
    }

    /// Returns the account address.
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns the chunk index.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Returns the loaded payload and its cached execution analysis.
    pub const fn get(&self) -> &CodeChunk {
        &self.chunk.chunk
    }

    /// Returns transaction-local warmth, independently of cached bytes.
    pub const fn is_warm(&self) -> bool {
        self.chunk.is_warm
    }

    /// Marks the chunk warm and journals the transition. Returns whether it was cold.
    pub fn warm(&mut self) -> bool {
        if self.is_warm() {
            return false;
        }
        self.chunk.is_warm = true;
        self.inner
            .journal
            .push(JournalEntry::CodeChunkWarmed { address: self.address, index: self.index });
        true
    }
}

impl<'db> State<'db> {
    /// Inspects logical warmth without loading metadata or bytes.
    pub fn code_chunk_is_warm(&self, address: &Address, index: u32) -> bool {
        self.accounts
            .get(address)
            .and_then(|account| account.code_chunks.get(&index))
            .is_some_and(|chunk| chunk.is_warm)
    }

    /// Loads a chunk and returns a handle without warming it or journaling a read.
    ///
    /// Like storage loading, successful reads remain cached across rollback. Call
    /// [`CodeChunkHandle::warm`] to record a reversible access. Callers must reserve gas
    /// before loading; `skip_cold_load` refuses cold accesses even when bytes are cached.
    /// Empty code and out-of-range indices return `None` without fetching a payload.
    pub fn load_code_chunk(
        &mut self,
        address: &Address,
        index: u32,
        skip_cold_load: bool,
    ) -> Result<Option<CodeChunkHandle<'_, 'db>>, LoadError> {
        let Self { accounts, inner, .. } = self;
        let account = Self::account_raw(inner, accounts, address)?;
        CodeChunkHandle::load(*address, index, account, inner, skip_cold_load)
    }
}

/// Fetches and validates a missing payload before publishing its execution analysis.
fn load_chunk(
    inner: &mut StateInner<'_>,
    info: &AccountInfo,
    index: u32,
    expected: Option<usize>,
) -> DbResult<CodeChunk> {
    let hash = info.code_hash;
    let multi = info.code_metadata().is_some();
    let result = if let Some(code) = &info.code {
        Ok(if multi {
            code_chunk(&code.original_bytes(), index)
        } else {
            Some(CodeChunk::from_bytecode(code))
        })
    } else if multi {
        inner.database.get_code_chunk_by_hash(&hash, index)
    } else {
        // Account type selects legacy representation even when the same hash also
        // belongs to an account using the fixed-stride chunk representation.
        inner.database.get_code_by_hash(&hash).map(|code| Some(CodeChunk::from_bytecode(&code)))
    };
    let chunk = match result {
        Ok(Some(chunk))
            if expected.map_or_else(
                || (1..=LEGACY_CODE_CHUNK_SIZE).contains(&chunk.original_bytes().len()),
                |length| {
                    chunk.original_bytes().len() == length
                        && info.code_metadata().is_some_and(|metadata| {
                            chunk.validate_context(metadata.code_size(), index).is_ok()
                        })
                },
            ) =>
        {
            chunk
        }
        result => {
            let (actual_length, reason, source) = match result {
                Ok(Some(chunk)) => {
                    (Some(chunk.original_bytes().len()), "invalid payload length", None)
                }
                Ok(None) => (None, "missing required payload", None),
                Err(error) => (None, "provider failure", Some(error)),
            };
            inner.database.discard_code_chunk(&hash, index);
            return Err(DatabaseError::new(
                CodeChunkError {
                    code_hash: hash,
                    index,
                    expected_length: expected,
                    actual_length,
                    reason,
                    source,
                },
                true,
            ));
        }
    };
    let _ = chunk.bytecode(multi).legacy_jump_table();
    Ok(chunk)
}
