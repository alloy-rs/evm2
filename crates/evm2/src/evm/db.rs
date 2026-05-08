//! Database helpers for the EVM state overlay.

use super::state::AccountInfo;
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::string::ToString;
use alloy_primitives::{Address, B256, keccak256};

mod cache;
pub use cache::{Cache, CacheDB, InMemoryDB};

/// Backing database view used to initialize mutable [`super::State`].
pub trait Database {
    /// Loads account information.
    fn get_account(&mut self, address: Address) -> Option<AccountInfo>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: B256) -> Bytecode;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: Address, key: Word) -> Word;

    /// Loads a historical block hash.
    fn get_block_hash(&mut self, number: Word) -> Option<B256>;
}

/// Empty backing database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyDB(());

impl Database for EmptyDB {
    #[inline]
    fn get_account(&mut self, _address: Address) -> Option<AccountInfo> {
        None
    }

    #[inline]
    fn get_code_by_hash(&mut self, _code_hash: B256) -> Bytecode {
        Bytecode::default()
    }

    #[inline]
    fn get_storage(&mut self, _address: Address, _key: Word) -> Word {
        Word::ZERO
    }

    #[inline]
    fn get_block_hash(&mut self, number: Word) -> Option<B256> {
        Some(keccak256(number.to_string().as_bytes()))
    }
}
