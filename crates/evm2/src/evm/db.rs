//! In-memory database helpers for the EVM state overlay.

use super::state::AccountInfo;
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::string::ToString;
use alloy_primitives::{Address, B256, KECCAK256_EMPTY, keccak256, map};

/// Backing database view used to initialize mutable [`super::State`].
pub trait Database {
    /// Loads account information.
    fn get_account(&self, address: Address) -> Option<AccountInfo>;

    /// Loads account code.
    fn get_account_code(&self, address: Address) -> Bytecode;

    /// Loads a persistent storage slot.
    fn get_storage(&self, address: Address, key: Word) -> Word;

    /// Loads a historical block hash.
    fn get_block_hash(&self, number: u64) -> Option<B256>;
}

/// A simple in-memory database view.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheDB {
    /// Accounts keyed by address.
    pub accounts: map::HashMap<Address, AccountInfo>,
    /// Contracts keyed by code hash.
    pub contracts: map::HashMap<B256, Bytecode>,
    /// Persistent storage keyed by account and slot.
    pub storage: map::HashMap<(Address, Word), Word>,
    /// Cached block hashes keyed by block number.
    pub block_hashes: map::HashMap<u64, B256>,
}

impl Default for CacheDB {
    #[inline]
    fn default() -> Self {
        let mut contracts = map::HashMap::default();
        contracts.insert(KECCAK256_EMPTY, Bytecode::default());
        contracts.insert(B256::ZERO, Bytecode::default());

        Self {
            accounts: map::HashMap::default(),
            contracts,
            storage: map::HashMap::default(),
            block_hashes: map::HashMap::default(),
        }
    }
}

impl CacheDB {
    /// Inserts account code into the contract cache.
    #[inline]
    pub fn insert_contract(&mut self, info: &mut AccountInfo) {
        if let Some(code) = &info.code
            && !code.is_empty()
        {
            if info.code_hash == KECCAK256_EMPTY {
                info.code_hash = code.hash_slow();
            }
            self.contracts.entry(info.code_hash).or_insert_with(|| code.clone());
        }
        if info.code_hash.is_zero() {
            info.code_hash = KECCAK256_EMPTY;
        }
    }

    /// Inserts account info.
    #[inline]
    pub fn insert_account_info(&mut self, address: Address, mut info: AccountInfo) {
        self.insert_contract(&mut info);
        self.accounts.insert(address, info);
    }

    /// Returns account info if the account exists.
    #[inline]
    pub fn account_info(&self, address: Address) -> Option<&AccountInfo> {
        self.accounts.get(&address)
    }

    /// Inserts persistent storage.
    #[inline]
    pub fn insert_account_storage(&mut self, address: Address, key: Word, value: Word) {
        self.accounts.entry(address).or_default();
        self.storage.insert((address, key), value);
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }
}

impl Database for CacheDB {
    #[inline]
    fn get_account(&self, address: Address) -> Option<AccountInfo> {
        self.accounts.get(&address).cloned()
    }

    #[inline]
    fn get_account_code(&self, address: Address) -> Bytecode {
        self.accounts
            .get(&address)
            .and_then(|info| info.code.clone())
            .or_else(|| {
                self.accounts
                    .get(&address)
                    .and_then(|info| self.contracts.get(&info.code_hash).cloned())
            })
            .unwrap_or_default()
    }

    #[inline]
    fn get_storage(&self, address: Address, key: Word) -> Word {
        self.storage.get(&(address, key)).copied().unwrap_or_default()
    }

    #[inline]
    fn get_block_hash(&self, number: u64) -> Option<B256> {
        self.block_hashes
            .get(&number)
            .copied()
            .or_else(|| Some(keccak256(number.to_string().as_bytes())))
    }
}

/// A database implementation that stores initial state in memory.
pub type InMemoryDB = CacheDB;
