//! Requested runtime analyses and checkpoint-local code warmth.

use super::{JournalEntry, State};
use crate::{
    CodeChunkError, DatabaseError, LoadError,
    bytecode::{Bytecode, CodeChunk, LEGACY_CODE_CHUNK_SIZE, code_chunk},
    evm::db::DynDatabase,
};
use alloy_primitives::{Address, KECCAK256_EMPTY};

/// One requested analysis, scoped to the account's current code identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountCodeChunk {
    /// Owned execution analysis; original bytes exclude padding.
    pub bytecode: Bytecode,
    /// Original payload and authenticated execution-only layout.
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

/// Cumulative local observations, retained across rollback and transaction reset.
/// Counters saturate and never participate in consensus decisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeChunkStats {
    /// Successful requests whose account entry was cold.
    pub logical_cold_accesses: u64,
    /// Successful requests whose account entry was already warm.
    pub logical_warm_accesses: u64,
    /// Successful requests served by resident or state-cached bytes.
    pub physical_cache_hits: u64,
    /// Calls from the state cache to its backing provider, including failures.
    pub physical_cache_misses: u64,
    /// Original payload bytes returned by backing calls, including malformed payloads.
    pub bytes_fetched: u64,
    /// Backing calls measured, including failures.
    pub read_latency_samples: u64,
    /// Saturating elapsed nanoseconds; zero without the std feature.
    pub read_latency_ns: u64,
    /// Newly constructed immutable execution analyses.
    pub analyzed_chunks: u64,
}

impl CodeChunkStats {
    /// Unlabelled aggregate metrics; no account or code identity is exported.
    pub const fn aggregate_metrics(&self) -> [(&'static str, u64); 8] {
        [
            ("logical_cold_accesses", self.logical_cold_accesses),
            ("logical_warm_accesses", self.logical_warm_accesses),
            ("physical_cache_hits", self.physical_cache_hits),
            ("physical_cache_misses", self.physical_cache_misses),
            ("bytes_fetched", self.bytes_fetched),
            ("read_latency_samples", self.read_latency_samples),
            ("read_latency_ns", self.read_latency_ns),
            ("analyzed_chunks", self.analyzed_chunks),
        ]
    }
}

impl State<'_> {
    /// Returns cumulative diagnostic samples without resetting them.
    pub fn code_chunk_stats(&self) -> CodeChunkStats {
        self.chunk_stats
    }

    /// Enables or disables future collection while retaining prior samples.
    pub fn set_code_chunk_diagnostics(&mut self, enabled: bool) {
        self.chunk_diagnostics = enabled;
    }

    fn record_chunk_access(&mut self, cold: bool, hit: bool) {
        if self.chunk_diagnostics {
            let stats = &mut self.inner.chunk_stats;
            let counter = if cold {
                &mut stats.logical_cold_accesses
            } else {
                &mut stats.logical_warm_accesses
            };
            *counter = counter.saturating_add(1);
            if hit {
                stats.physical_cache_hits = stats.physical_cache_hits.saturating_add(1);
            }
        }
    }

    /// Inspects logical warmth without loading metadata or bytes.
    pub fn code_chunk_is_warm(&self, address: &Address, index: u32) -> bool {
        self.accounts
            .get(address)
            .and_then(|account| account.code_chunks.get(&index))
            .is_some_and(|chunk| chunk.is_warm)
    }

    /// Loads one bounded payload and journals successful cold access.
    ///
    /// The interpreter must reserve the entire operation's chunk gas before calling this
    /// method. `skip_cold_load` refuses cold logical accesses even when bytes are cached.
    /// Provider failures and invalid lengths never publish analyses or warmth.
    pub fn load_code_chunk(
        &mut self,
        address: &Address,
        index: u32,
        skip_cold_load: bool,
    ) -> Result<Option<CodeChunkLoad>, LoadError> {
        let (hash, expected, multi, resident, total_size) = {
            let account = self.account(address)?;
            let hash = account.code_hash();
            if hash.is_zero() || hash == KECCAK256_EMPTY {
                return Ok(None);
            }
            let info = account.get().expect("nonempty code belongs to a present account");
            let expected = if let Some(metadata) = &info.code_metadata {
                let Some(length) = metadata.chunk_len(index) else { return Ok(None) };
                Some(length)
            } else {
                if index != 0 {
                    return Ok(None);
                }
                None
            };
            (
                hash,
                expected,
                info.code_metadata.is_some(),
                info.code.clone(),
                info.code_metadata.as_ref().map(|metadata| metadata.code_size()),
            )
        };
        let is_cold = !self.code_chunk_is_warm(address, index);
        if skip_cold_load && is_cold {
            return Err(LoadError::ColdLoadSkipped);
        }
        if let Some(entry) =
            self.accounts.get_mut(address).and_then(|account| account.code_chunks.get_mut(&index))
        {
            let chunk = entry.chunk.clone();
            entry.is_warm = true;
            if is_cold {
                self.journal.push(JournalEntry::CodeChunkWarmed { address: *address, index });
            }
            self.record_chunk_access(is_cold, true);
            return Ok(Some(CodeChunkLoad { chunk, is_cold }));
        }
        let cache_hit = resident.is_some()
            || (multi && self.database.cache.code_chunks.contains_key(&(hash, index)))
            || (!multi && self.database.cache.contracts.contains_key(&hash));
        #[cfg(feature = "std")]
        let started = std::time::Instant::now();
        let result = if let Some(code) = resident {
            Ok(if multi {
                code_chunk(&code.original_bytes(), index)
            } else {
                Some(CodeChunk::from_bytecode(&code))
            })
        } else if multi {
            self.database.get_code_chunk_by_hash(&hash, index)
        } else {
            // Account type selects unchanged legacy representation even when another account
            // with the same original hash uses the new fixed-stride representation.
            self.database.get_code_by_hash(&hash).map(|code| Some(CodeChunk::from_bytecode(&code)))
        };
        if self.chunk_diagnostics && !cache_hit {
            let stats = &mut self.inner.chunk_stats;
            stats.physical_cache_misses = stats.physical_cache_misses.saturating_add(1);
            stats.read_latency_samples = stats.read_latency_samples.saturating_add(1);
            #[cfg(feature = "std")]
            {
                stats.read_latency_ns = stats.read_latency_ns.saturating_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                );
            }
            if let Ok(Some(chunk)) = &result {
                stats.bytes_fetched =
                    stats.bytes_fetched.saturating_add(chunk.original_bytes().len() as u64);
            }
        }
        let mut chunk = match result {
            Ok(Some(chunk))
                if expected.map_or_else(
                    || (1..=LEGACY_CODE_CHUNK_SIZE).contains(&chunk.original_bytes().len()),
                    |length| {
                        chunk.original_bytes().len() == length
                            && total_size
                                .is_some_and(|size| chunk.validate_context(size, index).is_ok())
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
                self.database.discard_code_chunk(&hash, index);
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
                )
                .into());
            }
        };
        if self.chunk_diagnostics
            && !self.database.cache.analyzed_code_chunks.contains_key(&(hash, index, multi))
        {
            self.inner.chunk_stats.analyzed_chunks =
                self.inner.chunk_stats.analyzed_chunks.saturating_add(1);
        }
        let bytecode = self
            .database
            .cache
            .analyzed_code_chunks
            .entry((hash, index, multi))
            .or_insert_with(|| {
                let bytecode = chunk.bytecode(multi);
                // Analyze only requested payloads, once per immutable cache entry.
                let _ = bytecode.legacy_jump_table();
                bytecode
            })
            .clone();
        chunk.retain_analysis(multi, bytecode.clone());
        let account = self.accounts.get_mut(address).expect("account loaded above");
        if !multi {
            account.code_size =
                Some(u32::try_from(chunk.original_bytes().len()).expect("bounded legacy size"));
        }
        account
            .code_chunks
            .insert(index, AccountCodeChunk { bytecode, chunk: chunk.clone(), is_warm: true });
        self.journal.push(JournalEntry::CodeChunkWarmed { address: *address, index });
        self.record_chunk_access(is_cold, cache_hit);
        Ok(Some(CodeChunkLoad { chunk, is_cold }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EvmFeatures,
        bytecode::{CODE_CHUNK_SIZE, code_metadata},
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
                code_metadata: code_metadata(&self.bytes).unwrap(),
                ..AccountInfo::default()
            }))
        }

        fn get_code_kind_by_hash(
            &mut self,
            _: &B256,
        ) -> Result<crate::bytecode::BytecodeKind, Self::Error> {
            Ok(crate::bytecode::BytecodeKind::Legacy)
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
            assert_eq!(state.load_code_chunk(&a, 0, true), Err(LoadError::ColdLoadSkipped));
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            assert!(state.load_code_chunk(&a, 0, false).unwrap().unwrap().is_cold);
            let checkpoint = state.checkpoint();
            assert!(!state.load_code_chunk(&a, 0, true).unwrap().unwrap().is_cold);
            assert!(state.load_code_chunk(&b, 0, false).unwrap().unwrap().is_cold);
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
            assert_eq!(state.database.cache.analyzed_code_chunks.len(), 1);
            state.clear_transaction_state();
            assert!(state.load_code_chunk(&a, 0, false).unwrap().unwrap().is_cold);
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            assert_eq!(state.database.cache.analyzed_code_chunks.len(), 1);
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
        assert!(state.database.cache.analyzed_code_chunks.is_empty());
        malformed.store(false, Ordering::SeqCst);
        assert!(state.load_code_chunk(&owner, 1, false).unwrap().unwrap().is_cold);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert!(state.account(&owner).unwrap().code_chunks()[&1].bytecode.is_legacy());
    }
    #[test]
    fn diagnostics_survive_reset_snapshot_and_toggle() {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut state = State::new(Db::new(Provider {
            bytes: Bytes::from(vec![0; 17]),
            reads: reads.clone(),
            malformed: Arc::new(AtomicBool::new(false)),
        }));
        let owner = Address::repeat_byte(0x44);
        let checkpoint = state.checkpoint();
        state.load_code_chunk(&owner, 0, false).unwrap();
        state.rollback(checkpoint, EvmFeatures::empty());
        let stats = state.code_chunk_stats();
        assert_eq!(stats.logical_cold_accesses, 1);
        assert_eq!(stats.physical_cache_misses, 1);
        assert_eq!(stats.bytes_fetched, 17);
        assert_eq!(stats.analyzed_chunks, 1);
        assert_eq!(stats.read_latency_samples, 1);
        state.load_code_chunk(&owner, 0, false).unwrap();
        state.load_code_chunk(&owner, 0, false).unwrap();
        let stats = state.code_chunk_stats();
        assert_eq!(stats.logical_cold_accesses, 2);
        assert_eq!(stats.logical_warm_accesses, 1);
        assert_eq!(stats.physical_cache_hits, 2);
        state.set_code_chunk_diagnostics(false);
        state.clear_transaction_state();
        state.load_code_chunk(&owner, 0, false).unwrap();
        assert_eq!(state.code_chunk_stats(), stats);
        let restored = state.clone();
        assert_eq!(restored.code_chunk_stats(), stats);
        assert!(!restored.chunk_diagnostics);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
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
            let result = state.load_code_chunk(&owner, 1, false);
            assert_eq!(result.is_err(), malformed);
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            let stats = state.code_chunk_stats();
            assert_eq!(stats.physical_cache_hits, 0);
            assert_eq!(stats.physical_cache_misses, 1);
            assert_eq!(stats.read_latency_samples, 1);
            assert_eq!(stats.bytes_fetched, u64::from(!malformed));
            assert_eq!(stats.logical_cold_accesses, u64::from(!malformed));
            assert_eq!(stats.analyzed_chunks, u64::from(!malformed));
            assert_eq!(state.code_chunk_is_warm(&owner, 1), !malformed);
            if !malformed {
                state.clear_transaction_state();
                state.load_code_chunk(&owner, 1, false).unwrap();
                assert_eq!(reads.load(Ordering::SeqCst), 1);
                let stats = state.code_chunk_stats();
                assert_eq!(stats.physical_cache_hits, 1);
                assert_eq!(stats.physical_cache_misses, 1);
                assert_eq!(stats.logical_cold_accesses, 2);
            }
        }
    }
}
