//! Transaction-scoped chunk warmth over a persistent, immutable byte cache.

use super::{JournalEntry, State};
use crate::{
    LoadError,
    bytecode::{Bytecode, chunks::CodeChunk},
    evm::db::DynDatabase,
};
use alloy_primitives::{Address, Bytes, KECCAK256_EMPTY};

/// A loaded code chunk and its warmth before this access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeChunkLoad {
    /// Original code bytes and PUSH-boundary metadata.
    pub chunk: CodeChunk,
    /// Whether this access first warmed the chunk in the current scope.
    pub is_cold: bool,
}

impl State<'_> {
    /// Loads a chunk of the account's current code without warming the account itself.
    /// Account access gas and EIP-7702 resolution are the caller's responsibility.
    ///
    /// With `skip_cold_load`, a cold access returns `ColdLoadSkipped` before chunk I/O or
    /// warming. A caller can try this first, reserve `code_chunk_gas(1)`, then retry with
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
        let key = (*address, code_hash, index);
        let is_cold = !self.warm_code_chunks.contains(&key);
        if is_cold && skip_cold_load {
            return Err(LoadError::ColdLoadSkipped);
        }
        let account_chunk = self
            .accounts
            .get(address)
            .and_then(|account| account.code_chunks.get(&index))
            .and_then(|code| {
                CodeChunk::new_validated(Bytes::copy_from_slice(
                    &code.original_byte_slice()[..expected_len],
                ))
            });
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
        if let Some(account) = self.accounts.get_mut(address) {
            account.code_chunks.insert(index, chunk.bytecode().clone());
        }
        if is_cold {
            self.warm_code_chunks.insert(key);
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

    #[test]
    fn rollback_keeps_bytes_but_restores_warmth() {
        let mut state = state();
        let address = Address::ZERO;
        let outer = state.checkpoint();
        assert!(state.load_code_chunk(&address, 0, false).unwrap().unwrap().is_cold);
        assert!(state.accounts[&address].code_chunks.contains_key(&0));
        let inner = state.checkpoint();
        assert!(!state.load_code_chunk(&address, 0, true).unwrap().unwrap().is_cold);
        assert!(state.load_code_chunk(&address, 1, false).unwrap().unwrap().is_cold);
        state.rollback(inner, EvmFeatures::empty());
        assert!(!state.load_code_chunk(&address, 0, true).unwrap().unwrap().is_cold);
        assert!(state.load_code_chunk(&address, 1, false).unwrap().unwrap().is_cold);
        state.rollback(outer, EvmFeatures::empty());
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
        assert!(state.warm_code_chunks.is_empty());
        state.initial_mut().downcast_mut::<ChunkDb>().unwrap().fail = false;
        assert!(state.load_code_chunk(&address, 2, false).unwrap().is_none());
        assert!(state.warm_code_chunks.is_empty());
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
            state.accounts[&address].code_chunks[&0].original_byte_slice().len(),
            CODE_CHUNK_SIZE + 1
        );
        let load = state.load_code_chunk(&address, 0, true).unwrap().unwrap();
        assert!(!load.is_cold);
        assert_eq!(load.chunk.bytes().len(), CODE_CHUNK_SIZE);
        assert_eq!(state.initial().downcast_ref::<ChunkDb>().unwrap().reads, 1);
    }
}
