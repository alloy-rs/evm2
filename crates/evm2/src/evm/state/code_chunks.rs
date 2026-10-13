//! Requested runtime analyses and checkpoint-local code warmth.

use super::{Account, AccountInfo, JournalEntry, State, StateInner};
use crate::{
    CodeChunkLoadError, DatabaseError, LoadError,
    bytecode::{CodeChunk, LEGACY_CODE_CHUNK_SIZE, code_chunk},
    evm::db::{DbResult, DynDatabase},
};
use alloy_primitives::{Address, KECCAK256_EMPTY};
use derive_where::derive_where;

/// A loaded code chunk with journaled access warmth, like a storage slot handle.
#[derive_where(Debug)]
pub struct CodeChunkHandle<'a, 'db> {
    address: Address,
    index: u32,
    chunk: CodeChunk,
    is_warm: &'a mut bool,
    #[derive_where(skip)]
    inner: &'a mut StateInner<'db>,
}

impl<'a, 'db> CodeChunkHandle<'a, 'db> {
    fn load(
        address: Address,
        index: u32,
        account: &'a mut Account,
        inner: &'a mut StateInner<'db>,
        skip_cold: bool,
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
        let index_usize = index as usize;
        if account.code_chunks.chunks.len() <= index_usize {
            if skip_cold {
                return Err(LoadError::ColdLoadSkipped);
            }
            account.code_chunks.chunks.resize_with(index_usize + 1, || None);
            account.code_chunks.warm.resize(index_usize + 1, false);
        }
        let slot = &mut account.code_chunks.chunks[index_usize];
        if slot.is_none() {
            if skip_cold {
                return Err(LoadError::ColdLoadSkipped);
            }
            let chunk = load_chunk(inner, info, index, expected)?;
            if expected.is_none() {
                account.code_size = Some(chunk.payload_len() as u32);
            }
            *slot = Some(chunk.bytecode());
        }
        let is_warm = &mut account.code_chunks.warm[index_usize];
        if skip_cold && !*is_warm {
            return Err(LoadError::ColdLoadSkipped);
        }
        let bytecode = slot.as_ref().expect("chunk loaded");
        let chunk = if let Some(metadata) = info.code_metadata() {
            CodeChunk::from_prepared(bytecode.clone(), metadata.code_size(), index)
                .expect("cached validated chunk")
        } else {
            CodeChunk::from_bytecode(bytecode)
        };
        Ok(Some(Self { address, index, chunk, is_warm, inner }))
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
        &self.chunk
    }

    /// Returns transaction-local warmth, independently of cached bytes.
    pub const fn is_warm(&self) -> bool {
        *self.is_warm
    }

    /// Marks the chunk warm and journals the transition. Returns whether it was cold.
    pub fn warm(&mut self) -> bool {
        if self.is_warm() {
            return false;
        }
        *self.is_warm = true;
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
            .and_then(|account| account.code_chunks.warm.get(index as usize))
            .copied()
            .unwrap_or(false)
    }

    /// Loads a chunk and returns a handle without warming it or journaling a read.
    ///
    /// Like storage loading, successful reads remain cached across rollback. Call
    /// [`CodeChunkHandle::warm`] to record a reversible access. Callers must reserve gas
    /// before loading. With `skip_cold`, cold chunks return [`LoadError::ColdLoadSkipped`]
    /// without fetching, caching, warming, or journaling, even if their bytes are resident.
    /// For a loaded account, empty code and out-of-range indices return `None`.
    /// An unseen account is skipped without loading its metadata when `skip_cold` is true.
    pub fn load_code_chunk(
        &mut self,
        address: &Address,
        index: u32,
        skip_cold: bool,
    ) -> Result<Option<CodeChunkHandle<'_, 'db>>, LoadError> {
        let Self { accounts, inner, .. } = self;
        let account = if skip_cold {
            // An unseen account cannot have a warm chunk. Avoid its metadata read too.
            accounts.get_mut(address).ok_or(LoadError::ColdLoadSkipped)?
        } else {
            Self::account_raw(inner, accounts, address)?
        };
        CodeChunkHandle::load(*address, index, account, inner, skip_cold)
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
    let metadata = info.code_metadata();
    let multi = metadata.is_some();
    let payload_hash =
        metadata.map_or(hash, |m| m.chunk_hash(index).expect("validated chunk index"));
    let result = if let Some(code) = &info.code {
        Ok(if multi {
            code_chunk(&code.original_bytes(), index)
        } else {
            Some(CodeChunk::from_bytecode(code))
        })
    } else if let Some(metadata) = metadata {
        inner.database.get_code_by_hash(&payload_hash).and_then(|bytecode| {
            if bytecode.is_empty() {
                return Ok(None);
            }
            if bytecode.hash_slow() != payload_hash {
                return Err(DatabaseError::new(CodeChunkLoadError::Invalid(payload_hash), true));
            }
            CodeChunk::from_prepared(bytecode, metadata.code_size(), index)
                .map(Some)
                .map_err(|_| DatabaseError::new(CodeChunkLoadError::Invalid(payload_hash), true))
        })
    } else {
        // Account type selects legacy representation even when the same hash also
        // belongs to an account using the fixed-stride chunk representation.
        inner.database.get_code_by_hash(&hash).map(|code| Some(CodeChunk::from_bytecode(&code)))
    };
    let chunk = match result {
        Ok(Some(chunk))
            if expected.map_or_else(
                || (1..=LEGACY_CODE_CHUNK_SIZE).contains(&chunk.payload_len()),
                |length| {
                    chunk.payload_len() == length
                        && chunk.as_bytecode().hash_slow() == payload_hash
                        && info.code_metadata().is_some_and(|metadata| {
                            chunk.validate_context(metadata.code_size(), index).is_ok()
                        })
                },
            ) =>
        {
            chunk
        }
        result => {
            return Err(match result {
                Ok(Some(_)) => DatabaseError::new(CodeChunkLoadError::Invalid(payload_hash), true),
                Ok(None) => DatabaseError::new(CodeChunkLoadError::Missing(payload_hash), true),
                Err(error) => error.into_fatal(),
            });
        }
    };
    Ok(chunk)
}
