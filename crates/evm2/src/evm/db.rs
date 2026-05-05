//! Database helpers for the EVM state overlay.

use super::state::AccountInfo;
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::string::ToString;
use alloy_primitives::{Address, B256, keccak256};

mod cache;
pub use cache::{Cache, CacheDB, InMemoryDB};

/// Empty backing database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyDB(());

/// Backing database view used to initialize mutable [`super::State`].
pub trait Database {
    /// Loads account information.
    fn get_account(&self, address: Address) -> Option<AccountInfo>;

    /// Loads account code.
    fn get_account_code(&self, address: Address) -> Bytecode;

    /// Loads a persistent storage slot.
    fn get_storage(&self, address: Address, key: Word) -> Word;

    /// Loads a historical block hash.
    fn get_block_hash(&self, number: Word) -> Option<B256>;
}

impl Database for EmptyDB {
    #[inline]
    fn get_account(&self, _address: Address) -> Option<AccountInfo> {
        None
    }

    #[inline]
    fn get_account_code(&self, _address: Address) -> Bytecode {
        Bytecode::default()
    }

    #[inline]
    fn get_storage(&self, _address: Address, _key: Word) -> Word {
        Word::ZERO
    }

    #[inline]
    fn get_block_hash(&self, number: Word) -> Option<B256> {
        Some(keccak256(number.to_string().as_bytes()))
    }
}
