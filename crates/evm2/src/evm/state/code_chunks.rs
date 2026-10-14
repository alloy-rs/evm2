//! Requested runtime analyses and checkpoint-local code warmth.

use super::{Account, AccountInfo, CodeChunkSlot, JournalEntry, State, StateInner};
use crate::{
    CodeChunkLoadError, DatabaseError, LoadError,
    bytecode::{CodeChunk, CodeManifest, LEGACY_CODE_CHUNK_SIZE},
    evm::db::{DbResult, DynDatabase},
};
use alloy_primitives::{Address, KECCAK256_EMPTY};
use derive_where::derive_where;

/// A loaded code chunk with journaled access warmth, like a storage slot handle.
#[derive_where(Debug)]
pub struct CodeChunkHandle<'a, 'db> {
    address: Address,
    index: u32,
    chunk: &'a CodeChunk,
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
        if skip_cold
            && (account.code_chunks.code_hash != Some(info.code_hash)
                || account.code_chunks.slots.get(index as usize).is_none_or(|slot| !slot.is_warm))
        {
            return Err(LoadError::ColdLoadSkipped);
        }
        account.code_chunks.ensure_code_hash(info.code_hash);
        if account.code_chunks.manifest.is_none() {
            let (manifest, chunks) = if let Some(code) = info.code.as_ref() {
                if code.hash_slow() != info.code_hash {
                    return Err(DatabaseError::new(
                        CodeChunkLoadError::Invalid(info.code_hash),
                        true,
                    )
                    .into());
                }
                crate::bytecode::build_code_artifacts(&code.original_bytes()).map_err(|_| {
                    DatabaseError::new(CodeChunkLoadError::Invalid(info.code_hash), true)
                })?
            } else {
                let Some(manifest) = inner.database.get_code_manifest(&info.code_hash)? else {
                    return Err(DatabaseError::new(
                        CodeChunkLoadError::Missing(info.code_hash),
                        true,
                    )
                    .into());
                };
                (manifest, Vec::new())
            };
            account.code_chunks.manifest = Some(manifest);
            if !chunks.is_empty() {
                account.code_chunks.slots = chunks
                    .into_iter()
                    .map(|chunk| CodeChunkSlot { chunk: Some(chunk), is_warm: false })
                    .collect();
            }
        }
        let manifest = account.code_chunks.manifest.as_ref().expect("manifest loaded").clone();
        let expected = if manifest.is_chunked() {
            let Some(length) = manifest.chunk_len(index) else { return Ok(None) };
            length
        } else {
            if index != 0 {
                return Ok(None);
            }
            manifest.code_size() as usize
        };
        let index_usize = index as usize;
        if account.code_chunks.slots.len() <= index_usize {
            if skip_cold {
                return Err(LoadError::ColdLoadSkipped);
            }
            account.code_chunks.slots.resize_with(index_usize + 1, CodeChunkSlot::default);
        }
        let slot = &mut account.code_chunks.slots[index_usize];
        if slot.chunk.is_none() {
            if skip_cold {
                return Err(LoadError::ColdLoadSkipped);
            }
            let chunk = load_chunk(inner, info, &manifest, index, expected)?;
            slot.chunk = Some(chunk);
        }
        if skip_cold && !slot.is_warm {
            return Err(LoadError::ColdLoadSkipped);
        }
        let chunk = slot.chunk.as_ref().expect("chunk loaded");
        if manifest.is_chunked() {
            let payload_hash = manifest.chunk_hash(index).expect("validated chunk index");
            if chunk.as_bytecode().hash_slow() != payload_hash
                || chunk.validate_context(manifest.code_size(), index).is_err()
            {
                return Err(
                    DatabaseError::new(CodeChunkLoadError::Invalid(payload_hash), true).into()
                );
            }
        }
        Ok(Some(Self { address, index, chunk, is_warm: &mut slot.is_warm, inner }))
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
        self.chunk
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
            .and_then(|account| account.code_chunks.slots.get(index as usize))
            .map(|slot| slot.is_warm)
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
    manifest: &CodeManifest,
    index: u32,
    expected: usize,
) -> DbResult<CodeChunk> {
    let hash = info.code_hash;
    let multi = manifest.is_chunked();
    let payload_hash = manifest.chunk_hash(index).unwrap_or(hash);
    let result = if let Some(code) = &info.code {
        Ok(if multi {
            crate::bytecode::build_code_artifacts(&code.original_bytes())
                .ok()
                .and_then(|(_, chunks)| chunks.into_iter().nth(index as usize))
        } else {
            Some(CodeChunk::from_legacy_bytecode(code))
        })
    } else if multi {
        inner.database.get_code_by_hash(&payload_hash).and_then(|bytecode| {
            if bytecode.is_empty() {
                return Ok(None);
            }
            if bytecode.hash_slow() != payload_hash {
                return Err(DatabaseError::new(CodeChunkLoadError::Invalid(payload_hash), true));
            }
            CodeChunk::from_chunked(bytecode, manifest.code_size(), index)
                .map(Some)
                .map_err(|_| DatabaseError::new(CodeChunkLoadError::Invalid(payload_hash), true))
        })
    } else {
        // Account type selects legacy representation even when the same hash also
        // belongs to an account using the fixed-stride chunk representation.
        inner
            .database
            .get_code_by_hash(&hash)
            .map(|code| Some(CodeChunk::from_legacy_bytecode(&code)))
    };
    let chunk = match result {
        Ok(Some(chunk))
            if chunk.payload_len() == expected
                && (multi
                    && chunk.as_bytecode().hash_slow() == payload_hash
                    && chunk.validate_context(manifest.code_size(), index).is_ok()
                    || !multi && (1..=LEGACY_CODE_CHUNK_SIZE).contains(&chunk.payload_len())) =>
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
