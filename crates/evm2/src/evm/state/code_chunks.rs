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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EvmFeatures,
        bytecode::{Bytecode, CODE_CHUNK_SIZE, code_metadata},
        evm::{AccountInfo, Database, Db},
        interpreter::Word,
    };
    use alloc::{sync::Arc, vec};
    use alloy_primitives::{B256, Bytes, keccak256};
    use core::{
        convert::Infallible,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Provider {
        bytes: Bytes,
        reads: Arc<AtomicUsize>,
        malformed: Arc<AtomicBool>,
    }

    impl Database for Provider {
        type Error = Infallible;

        fn get_account(&mut self, _: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(Some(AccountInfo {
                code_hash: keccak256(&self.bytes),
                extension: (code_metadata(&self.bytes).unwrap())
                    .map(crate::evm::AccountExtension::chunked)
                    .unwrap_or_default(),
                ..AccountInfo::default()
            }))
        }

        fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
            assert!(self.bytes.len() <= LEGACY_CODE_CHUNK_SIZE);
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(Bytecode::new_legacy(self.bytes.clone()))
        }

        fn get_code_chunk_by_hash(
            &mut self,
            _: &B256,
            index: u32,
        ) -> Result<Option<CodeChunk>, Self::Error> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.malformed.load(Ordering::SeqCst) {
                Ok(Some(CodeChunk::new(Bytes::new())))
            } else {
                Ok(code_chunk(&self.bytes, index))
            }
        }

        fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
            Ok(Word::ZERO)
        }

        fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    #[test]
    fn sparse_state_bounds_warmth_and_retained_analysis() {
        for multi in [false, true] {
            let bytes = Bytes::from(vec![0; if multi { CODE_CHUNK_SIZE + 1 } else { 17 }]);
            let reads = Arc::new(AtomicUsize::new(0));
            let mut state = State::new(Db::new(Provider {
                bytes: bytes.clone(),
                reads: reads.clone(),
                malformed: Arc::new(AtomicBool::new(false)),
            }));
            let a = Address::repeat_byte(0x44);
            let b = Address::repeat_byte(0x45);
            assert_eq!(
                state.account(&a).unwrap().code_size(),
                if multi { Some(bytes.len() as u32) } else { None }
            );
            assert!(state.load_code_chunk(&a, u32::MAX, false).unwrap().is_none());
            assert!(matches!(state.load_code_chunk(&a, 0, true), Err(LoadError::ColdLoadSkipped)));
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            assert!(state.load_code_chunk(&a, 0, false).unwrap().unwrap().warm());
            let checkpoint = state.checkpoint();
            assert!(!state.load_code_chunk(&a, 0, true).unwrap().unwrap().warm());
            assert!(state.load_code_chunk(&b, 0, false).unwrap().unwrap().warm());
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            state.rollback(checkpoint, EvmFeatures::empty());
            assert!(state.code_chunk_is_warm(&a, 0));
            assert!(!state.code_chunk_is_warm(&b, 0));
            let checkpoint = state.checkpoint();
            state.account(&a).unwrap().set_code_slow(Bytecode::default());
            assert!(state.account(&a).unwrap().code_chunks().is_empty());
            assert_eq!(state.account(&a).unwrap().code_size(), Some(0));
            state.rollback(checkpoint, EvmFeatures::empty());
            assert!(state.code_chunk_is_warm(&a, 0));
            state.clear_transaction_state();
            assert!(state.load_code_chunk(&a, 0, false).unwrap().unwrap().warm());
            assert_eq!(reads.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn malformed_chunks_are_evicted_before_repair() {
        let mut bytes = vec![0; CODE_CHUNK_SIZE];
        bytes.extend_from_slice(&[0xef, 1]);
        let reads = Arc::new(AtomicUsize::new(0));
        let malformed = Arc::new(AtomicBool::new(true));
        let mut state = State::new(Db::new(Provider {
            bytes: bytes.into(),
            reads: reads.clone(),
            malformed: malformed.clone(),
        }));
        let owner = Address::repeat_byte(0x44);
        let Err(LoadError::Database(error)) = state.load_code_chunk(&owner, 1, false) else {
            panic!("wrong length must fail")
        };
        let error = error.downcast_ref::<CodeChunkError>().unwrap();
        assert_eq!(error.expected_length, Some(2));
        assert_eq!(error.actual_length, Some(0));
        assert!(state.account(&owner).unwrap().code_chunks().is_empty());
        assert!(state.database.cache.code_chunks.is_empty());
        malformed.store(false, Ordering::SeqCst);
        assert!(state.load_code_chunk(&owner, 1, false).unwrap().unwrap().warm());
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert!(state.account(&owner).unwrap().code_chunks()[&1].chunk.bytecode(true).is_legacy());
    }

    #[test]
    fn full_code_residency_does_not_hide_sparse_provider_reads() {
        for malformed in [false, true] {
            let bytes = Bytes::from(vec![0; CODE_CHUNK_SIZE + 1]);
            let hash = keccak256(&bytes);
            let reads = Arc::new(AtomicUsize::new(0));
            let mut state = State::new(Db::new(Provider {
                bytes: bytes.clone(),
                reads: reads.clone(),
                malformed: Arc::new(AtomicBool::new(malformed)),
            }));
            // Full-code RPC residency is not publication of executable chunks.
            state.database.cache.contracts.insert(hash, Bytecode::new_legacy(bytes));
            let owner = Address::repeat_byte(0x44);
            let result = state
                .load_code_chunk(&owner, 1, false)
                .map(|chunk| chunk.map(|mut chunk| chunk.warm()));
            assert_eq!(result.is_err(), malformed);
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            assert_eq!(state.code_chunk_is_warm(&owner, 1), !malformed);
            if !malformed {
                state.clear_transaction_state();
                state.load_code_chunk(&owner, 1, false).unwrap();
                assert_eq!(reads.load(Ordering::SeqCst), 1);
            }
        }
    }

    #[test]
    fn loading_is_read_only_and_warming_is_reversible() {
        for multi in [false, true] {
            let reads = Arc::new(AtomicUsize::new(0));
            let mut state = State::new(Db::new(Provider {
                bytes: Bytes::from(vec![0; if multi { CODE_CHUNK_SIZE + 1 } else { 17 }]),
                reads: reads.clone(),
                malformed: Arc::new(AtomicBool::new(false)),
            }));
            let owner = Address::repeat_byte(0x44);
            let checkpoint = state.checkpoint();
            for _ in 0..2 {
                let chunk = state.load_code_chunk(&owner, 0, false).unwrap().unwrap();
                assert_eq!(chunk.address(), owner);
                assert_eq!(chunk.index(), 0);
                assert_eq!(
                    chunk.get().original_bytes().len(),
                    if multi { CODE_CHUNK_SIZE } else { 17 }
                );
                assert!(!chunk.is_warm());
            }
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            assert_eq!(state.checkpoint(), checkpoint);
            assert!(!state.account(&owner).unwrap().is_warm());
            assert!(!state.account(&owner).unwrap().is_touched());
            assert!(matches!(
                state.load_code_chunk(&owner, 0, true),
                Err(LoadError::ColdLoadSkipped)
            ));
            assert_eq!(state.checkpoint(), checkpoint);

            {
                let mut chunk = state.load_code_chunk(&owner, 0, false).unwrap().unwrap();
                assert!(chunk.warm());
                assert!(chunk.is_warm());
                assert!(!chunk.warm());
            }
            assert_eq!(state.checkpoint().journal_len(), checkpoint.journal_len() + 1);
            assert!(state.clone().code_chunk_is_warm(&owner, 0));
            state.rollback(checkpoint.clone(), EvmFeatures::empty());
            assert_eq!(state.checkpoint(), checkpoint);
            assert!(!state.code_chunk_is_warm(&owner, 0));
            assert!(matches!(
                state.load_code_chunk(&owner, 0, true),
                Err(LoadError::ColdLoadSkipped)
            ));
            assert!(state.load_code_chunk(&owner, 0, false).unwrap().unwrap().warm());
            assert_eq!(reads.load(Ordering::SeqCst), 1);
        }
    }
}
