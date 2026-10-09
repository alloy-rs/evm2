//! Transaction-scoped chunk warmth over a persistent, immutable byte cache.

use super::{AccountCodeChunk, DbResult, JournalEntry, State};
use crate::{
    LoadError,
    bytecode::{Bytecode, chunks::CodeChunk},
    constants::EIP7702_BYTECODE_LEN,
    evm::db::DynDatabase,
};
use alloy_primitives::{Address, KECCAK256_EMPTY};

/// A loaded code chunk and its warmth before this access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeChunkLoad {
    /// Original code bytes and PUSH-boundary metadata.
    pub chunk: CodeChunk,
    /// Whether this access first warmed the chunk in the current scope.
    pub is_cold: bool,
}

impl State<'_> {
    /// Returns whether this account's current chunk is warm without performing database I/O.
    pub fn code_chunk_is_warm(&self, address: &Address, index: u32) -> bool {
        let Some(account) = self.accounts.get(address) else {
            return false;
        };
        let Some(code_hash) = account.present.as_ref().map(|info| info.code_hash) else {
            return false;
        };
        account
            .code_chunks
            .get(&index)
            .is_some_and(|chunk| chunk.code_hash == code_hash && chunk.is_warm)
    }

    /// Checks whether the account's current code is an EIP-7702 designator, loading only chunk
    /// zero when size metadata alone cannot reject it.
    pub fn current_code_is_eip7702(&mut self, address: &Address) -> DbResult<bool> {
        let account = self.account(address)?;
        let Some(info) = account.get() else {
            return Ok(false);
        };
        let code_size = if info.code_size == 0 {
            info.code.as_ref().map_or(0, Bytecode::len)
        } else {
            info.code_size as usize
        };
        if code_size != EIP7702_BYTECODE_LEN {
            return Ok(false);
        }
        if let Some(code) = info.code.as_ref() {
            return Ok(code.is_eip7702());
        }
        drop(account);
        let load = self.load_code_chunk(address, 0, false).map_err(|error| match error {
            LoadError::Database(error) => error,
            LoadError::ColdLoadSkipped => unreachable!("cold-load skipping is disabled"),
        })?;
        Ok(load.is_some_and(|load| load.chunk.bytecode().is_eip7702()))
    }

    /// Checks whether the account's transaction-boundary code was an EIP-7702 designator,
    /// loading only its first chunk when necessary. Original-code inspection does not affect the
    /// warmth of the account's current code identity.
    pub fn original_code_is_eip7702(&mut self, address: &Address) -> DbResult<bool> {
        drop(self.account(address)?);
        let Some(info) = self.accounts.get(address).and_then(|account| account.original.as_ref())
        else {
            return Ok(false);
        };
        let code_size = if info.code_size == 0 {
            info.code.as_ref().map_or(0, Bytecode::len)
        } else {
            info.code_size as usize
        };
        if code_size != EIP7702_BYTECODE_LEN {
            return Ok(false);
        }
        if let Some(code) = info.code.as_ref() {
            return Ok(code.is_eip7702());
        }
        let code_hash = info.code_hash;
        Ok(self
            .database
            .get_code_chunk_by_hash(&code_hash, 0)?
            .is_some_and(|chunk| chunk.bytecode().is_eip7702()))
    }

    /// Loads a chunk of the account's current code without warming the account itself.
    /// Account access gas and EIP-7702 resolution are the caller's responsibility.
    ///
    /// With `skip_cold_load`, a cold access returns `ColdLoadSkipped` before chunk I/O or
    /// warming. A caller can try this first, reserve `code_chunk_gas(1, 0)`, then retry with
    /// `false`. Empty code returns `None`. Callers must clip ranges to trusted code length;
    /// this API does not provide code-size metadata or charge gas itself.
    /// Cached bytes survive rollback; warmth rolls back and resets at transaction boundaries.
    pub fn load_code_chunk(
        &mut self,
        address: &Address,
        index: u32,
        skip_cold_load: bool,
    ) -> Result<Option<CodeChunkLoad>, LoadError> {
        let account = self.account(address)?;
        let code_hash = account.code_hash();
        if code_hash == KECCAK256_EMPTY {
            return Ok(None);
        }
        let (code_size, resident) = account
            .get()
            .map(|info| {
                let code_size = if info.code_size == 0 {
                    info.code.as_ref().map_or(0, Bytecode::len)
                } else {
                    info.code_size as usize
                };
                (code_size, info.code.clone())
            })
            .unwrap_or_default();
        drop(account);
        let chunk_start = index as usize * crate::bytecode::chunks::CODE_CHUNK_SIZE;
        if chunk_start >= code_size {
            return Ok(None);
        }
        let expected_len = (code_size - chunk_start).min(crate::bytecode::chunks::CODE_CHUNK_SIZE);
        let is_cold = self
            .accounts
            .get(address)
            .and_then(|account| account.code_chunks.get(&index))
            .is_none_or(|chunk| chunk.code_hash != code_hash || !chunk.is_warm);
        if is_cold && skip_cold_load {
            return Err(LoadError::ColdLoadSkipped);
        }
        let account_chunk = self
            .accounts
            .get(address)
            .and_then(|account| account.code_chunks.get(&index))
            .filter(|chunk| chunk.code_hash == code_hash)
            .map(|chunk| chunk.code.clone());
        let chunk = if let Some(chunk) = account_chunk {
            Some(chunk)
        } else if let Some(code) = resident {
            // Uncommitted deployment: never resolve its bytes through the accepted-state DB.
            let chunk = code.code_chunk(index);
            self.database.cache.code_chunks.insert((code_hash, index), chunk.clone());
            chunk
        } else {
            self.database.get_code_chunk_by_hash(&code_hash, index)?
        };
        let Some(chunk) = chunk else {
            return Ok(None);
        };
        if chunk.bytes().len() != expected_len {
            return Ok(None);
        }
        if is_cold {
            let account = self.accounts.get_mut(address).expect("account was loaded above");
            account
                .code_chunks
                .entry(index)
                .and_modify(|loaded| {
                    loaded.code_hash = code_hash;
                    loaded.code = chunk.clone();
                    loaded.is_warm = true;
                })
                .or_insert_with(|| AccountCodeChunk {
                    code_hash,
                    code: chunk.clone(),
                    is_warm: true,
                });
            self.journal.push(JournalEntry::CodeChunkWarmed {
                address: *address,
                code_hash,
                index,
            });
        }
        Ok(Some(CodeChunkLoad { chunk, is_cold }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DatabaseError, EvmFeatures,
        bytecode::{
            Bytecode,
            chunks::{CODE_CHUNK_SIZE, chunkify_code},
        },
        evm::{
            AccountInfo,
            db::{DbResult, EmptyDB},
        },
        interpreter::Word,
    };
    use alloc::{vec, vec::Vec};
    use alloy_primitives::B256;

    #[derive(Default)]
    struct ChunkDb {
        chunks: Vec<CodeChunk>,
        reads: usize,
        fail: bool,
    }

    impl DynDatabase for ChunkDb {
        fn get_account(&mut self, _: &Address) -> DbResult<Option<AccountInfo>> {
            Ok(Some(AccountInfo {
                code_hash: B256::with_last_byte(1),
                code_size: (CODE_CHUNK_SIZE + 5) as u32,
                ..Default::default()
            }))
        }
        fn get_code_by_hash(&mut self, _: &B256) -> DbResult<Bytecode> {
            panic!("full-code fetch")
        }
        fn get_code_chunk_by_hash(&mut self, _: &B256, index: u32) -> DbResult<Option<CodeChunk>> {
            self.reads += 1;
            if self.fail {
                return Err(DatabaseError::new(core::fmt::Error, true));
            }
            Ok(self.chunks.get(index as usize).cloned())
        }
        fn get_storage(&mut self, _: &Address, _: &Word) -> DbResult<Word> {
            unreachable!()
        }
        fn get_block_hash(&mut self, _: &Word) -> DbResult<B256> {
            unreachable!()
        }
    }

    fn state() -> State<'static> {
        State::new(ChunkDb {
            chunks: {
                let mut code = vec![0x5b; CODE_CHUNK_SIZE + 5];
                code[CODE_CHUNK_SIZE - 1] = 0x00;
                chunkify_code(&code).unwrap()
            },
            ..Default::default()
        })
    }

    struct SparseChunkDb {
        code_size: usize,
        reads: Vec<u32>,
    }

    impl DynDatabase for SparseChunkDb {
        fn get_account(&mut self, _: &Address) -> DbResult<Option<AccountInfo>> {
            Ok(Some(AccountInfo {
                code_hash: B256::with_last_byte(2),
                code_size: self.code_size as u32,
                ..Default::default()
            }))
        }

        fn get_code_by_hash(&mut self, _: &B256) -> DbResult<Bytecode> {
            panic!("sparse execution must never fetch complete bytecode")
        }

        fn get_code_chunk_by_hash(&mut self, _: &B256, index: u32) -> DbResult<Option<CodeChunk>> {
            self.reads.push(index);
            let start = index as usize * CODE_CHUNK_SIZE;
            if start >= self.code_size {
                return Ok(None);
            }
            let len = (self.code_size - start).min(CODE_CHUNK_SIZE);
            let mut payload = vec![crate::interpreter::op::JUMPDEST; len];
            if start + len < self.code_size {
                *payload.last_mut().unwrap() = crate::interpreter::op::STOP;
            }
            Ok(CodeChunk::new_validated(payload.into()))
        }

        fn get_storage(&mut self, _: &Address, _: &Word) -> DbResult<Word> {
            unreachable!()
        }

        fn get_block_hash(&mut self, _: &Word) -> DbResult<B256> {
            unreachable!()
        }
    }

    #[test]
    fn huge_sparse_code_reads_only_the_requested_database_chunk() {
        const DISTANT_INDEX: u32 = 4095;
        let code_size = (DISTANT_INDEX as usize + 1) * CODE_CHUNK_SIZE + 17;
        let mut state = State::new(SparseChunkDb { code_size, reads: Vec::new() });
        let address = Address::ZERO;

        assert!(state.account(&address).unwrap().exists());
        assert!(state.initial().downcast_ref::<SparseChunkDb>().unwrap().reads.is_empty());
        assert!(state.accounts[&address].present.as_ref().unwrap().code.is_none());

        let load = state.load_code_chunk(&address, DISTANT_INDEX, false).unwrap().unwrap();
        assert!(load.is_cold);
        assert_eq!(load.chunk.bytes().len(), CODE_CHUNK_SIZE);
        assert_eq!(state.initial().downcast_ref::<SparseChunkDb>().unwrap().reads, [DISTANT_INDEX]);
        assert!(!state.accounts[&address].code_chunks.contains_key(&0));
        assert_eq!(state.accounts[&address].code_chunks.len(), 1);

        assert!(!state.load_code_chunk(&address, DISTANT_INDEX, false).unwrap().unwrap().is_cold);
        assert_eq!(state.initial().downcast_ref::<SparseChunkDb>().unwrap().reads, [DISTANT_INDEX]);
    }

    #[test]
    fn account_and_other_chunks_do_not_eagerly_load_chunk_zero() {
        let mut state = state();
        let address = Address::ZERO;

        assert!(state.account(&address).unwrap().exists());
        assert!(state.accounts[&address].code_chunks.is_empty());
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 0);

        let load = state.load_code_chunk(&address, 1, false).unwrap().unwrap();
        assert!(load.is_cold);
        assert!(state.accounts[&address].code_chunks[&1].is_warm);
        assert!(!state.accounts[&address].code_chunks.contains_key(&0));
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 1);

        let load = state.load_code_chunk(&address, 1, false).unwrap().unwrap();
        assert!(!load.is_cold);
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 1);
    }

    #[test]
    fn rollback_keeps_bytes_but_restores_warmth() {
        let mut state = state();
        let address = Address::ZERO;
        let outer = state.checkpoint();
        assert!(state.load_code_chunk(&address, 0, false).unwrap().unwrap().is_cold);
        assert!(state.accounts[&address].code_chunks[&0].is_warm);
        let inner = state.checkpoint();
        assert!(!state.load_code_chunk(&address, 0, true).unwrap().unwrap().is_cold);
        assert!(state.load_code_chunk(&address, 1, false).unwrap().unwrap().is_cold);
        state.rollback(inner, EvmFeatures::empty());
        assert!(!state.load_code_chunk(&address, 0, true).unwrap().unwrap().is_cold);
        assert!(!state.accounts[&address].code_chunks[&1].is_warm);
        assert!(state.load_code_chunk(&address, 1, false).unwrap().unwrap().is_cold);
        state.rollback(outer, EvmFeatures::empty());
        assert!(!state.accounts[&address].code_chunks[&0].is_warm);
        assert!(state.load_code_chunk(&address, 0, false).unwrap().unwrap().is_cold);
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 2);
        state.clear_transaction_state();
        assert!(state.load_code_chunk(&address, 0, false).unwrap().unwrap().is_cold);
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 2);
    }

    #[test]
    fn skipped_failed_and_absent_reads_do_not_warm() {
        let mut state = state();
        let address = Address::ZERO;
        assert!(matches!(
            state.load_code_chunk(&address, 0, true),
            Err(LoadError::ColdLoadSkipped)
        ));
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 0);
        state.initial_mut().downcast_mut::<ChunkDb>().unwrap().fail = true;
        assert!(matches!(state.load_code_chunk(&address, 0, false), Err(LoadError::Database(_))));
        assert!(state.accounts[&address].code_chunks.is_empty());
        state.initial_mut().downcast_mut::<ChunkDb>().unwrap().fail = false;
        assert!(state.load_code_chunk(&address, 2, false).unwrap().is_none());
        assert!(state.accounts[&address].code_chunks.is_empty());
        assert!(state.load_code_chunk(&address, 0, false).unwrap().unwrap().is_cold);
    }

    #[test]
    fn snapshot_preserves_cache_warmth_and_journal_and_addresses_are_distinct() {
        let mut state = state();
        let checkpoint = state.checkpoint();
        state.load_code_chunk(&Address::ZERO, 0, false).unwrap();
        let mut restored = state.snapshot().into_state(EmptyDB::default());
        assert!(!restored.load_code_chunk(&Address::ZERO, 0, true).unwrap().unwrap().is_cold);
        restored.rollback(checkpoint, EvmFeatures::empty());
        assert!(restored.load_code_chunk(&Address::ZERO, 0, false).unwrap().unwrap().is_cold);
        assert!(
            state.load_code_chunk(&Address::with_last_byte(2), 0, false).unwrap().unwrap().is_cold
        );
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 1);
    }
    #[test]
    fn deployment_and_replacement_use_current_code_and_revert_identity() {
        let mut state = state();
        let address = Address::ZERO;
        state.load_code_chunk(&address, 0, false).unwrap();
        let checkpoint = state.checkpoint();
        state
            .account(&address)
            .unwrap()
            .set_code_slow(Bytecode::new_legacy(vec![0x00, 0x5b].into()));
        let load = state.load_code_chunk(&address, 0, false).unwrap().unwrap();
        assert!(load.is_cold);
        assert_eq!(load.chunk.bytes(), &[0x00, 0x5b]);
        state.rollback(checkpoint, EvmFeatures::empty());
        assert_eq!(
            state.accounts[&address].code_chunks[&0].code.bytecode().original_byte_slice().len(),
            CODE_CHUNK_SIZE + 1
        );
        let load = state.load_code_chunk(&address, 0, true).unwrap().unwrap();
        assert!(!load.is_cold);
        assert_eq!(load.chunk.bytes().len(), CODE_CHUNK_SIZE);
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 1);
    }

    #[test]
    fn direct_account_info_code_replacement_cannot_reuse_stale_chunk_or_warmth() {
        let mut state = state();
        let address = Address::ZERO;
        let old = state.load_code_chunk(&address, 0, false).unwrap().unwrap();
        assert_eq!(old.chunk.bytes()[0], crate::interpreter::op::JUMPDEST);

        let mut replacement = vec![crate::interpreter::op::JUMPDEST; CODE_CHUNK_SIZE + 5];
        replacement[0] = crate::interpreter::op::STOP;
        replacement[CODE_CHUNK_SIZE - 1] = crate::interpreter::op::STOP;
        state
            .account(&address)
            .unwrap()
            .get_or_insert()
            .set_code(Bytecode::new_legacy(replacement.into()));

        assert!(matches!(
            state.load_code_chunk(&address, 0, true),
            Err(LoadError::ColdLoadSkipped)
        ));
        let replacement = state.load_code_chunk(&address, 0, false).unwrap().unwrap();
        assert!(replacement.is_cold);
        assert_eq!(replacement.chunk.bytes()[0], crate::interpreter::op::STOP);
    }
}
