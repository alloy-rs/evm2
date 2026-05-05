//! In-memory cache database.

use super::{Database, EmptyDB};
use crate::{KECCAK_EMPTY, bytecode::Bytecode, evm::state::AccountInfo, interpreter::Word};
use alloy_primitives::{
    Address, B256,
    map::{AddressMap, B256Map, HashMap, U256Map},
};

/// A database implementation that stores initial state in memory.
pub type InMemoryDB = CacheDB<EmptyDB>;

/// Cache used by [`CacheDB`].
///
/// Accounts and code are stored separately: accounts carry the code hash, and
/// bytecode is keyed by that hash in [`Self::contracts`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cache {
    /// Accounts keyed by address.
    pub accounts: AddressMap<AccountInfo>,
    /// Contracts keyed by code hash.
    pub contracts: B256Map<Bytecode>,
    /// Persistent storage keyed by account and slot.
    pub storage: HashMap<(Address, Word), Word>,
    /// Cached block hashes keyed by block number.
    pub block_hashes: U256Map<B256>,
}

impl Default for Cache {
    #[inline]
    fn default() -> Self {
        let mut contracts = B256Map::default();
        contracts.insert(KECCAK_EMPTY, Bytecode::default());
        contracts.insert(B256::ZERO, Bytecode::default());
        Self {
            accounts: AddressMap::default(),
            contracts,
            storage: HashMap::default(),
            block_hashes: U256Map::default(),
        }
    }
}

/// A cache database over another backing database.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheDB<ExtDB = EmptyDB> {
    /// The cache that stores all local state.
    pub cache: Cache,
    /// Wrapped backing database.
    pub db: ExtDB,
}

impl Default for CacheDB<EmptyDB> {
    #[inline]
    fn default() -> Self {
        Self::new(EmptyDB::default())
    }
}

impl<ExtDB> CacheDB<ExtDB> {
    /// Creates a new cache over a backing database.
    #[inline]
    pub fn new(db: ExtDB) -> Self {
        Self { cache: Cache::default(), db }
    }

    /// Inserts account code into the contract cache.
    #[inline]
    pub fn insert_contract(&mut self, info: &AccountInfo) {
        if let Some(code) = &info.code
            && !code.is_empty()
        {
            self.cache.contracts.entry(info.code_hash).or_insert_with(|| code.clone());
        }
    }

    /// Inserts account info.
    #[inline]
    pub fn insert_account_info(&mut self, address: Address, mut info: AccountInfo) {
        if let Some(code) = &info.code
            && !code.is_empty()
            && info.code_hash == KECCAK_EMPTY
        {
            info.code_hash = code.hash_slow();
        }
        if info.code_hash.is_zero() {
            info.code_hash = KECCAK_EMPTY;
        }
        self.insert_contract(&info);
        self.cache.accounts.insert(address, info);
    }

    /// Returns cached account info if the account exists in the cache.
    #[inline]
    pub fn account_info(&self, address: Address) -> Option<&AccountInfo> {
        self.cache.accounts.get(&address)
    }

    /// Inserts persistent storage.
    #[inline]
    pub fn insert_account_storage(&mut self, address: Address, key: Word, value: Word) {
        self.cache.accounts.entry(address).or_default();
        self.cache.storage.insert((address, key), value);
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: Word, hash: B256) {
        self.cache.block_hashes.insert(number, hash);
    }
}

impl<ExtDB: Database> Database for CacheDB<ExtDB> {
    #[inline]
    fn get_account(&self, address: Address) -> Option<AccountInfo> {
        self.cache.accounts.get(&address).cloned().or_else(|| self.db.get_account(address))
    }

    #[inline]
    fn get_account_code(&self, address: Address) -> Bytecode {
        self.cache
            .accounts
            .get(&address)
            .and_then(|info| info.code.clone())
            .or_else(|| {
                self.cache
                    .accounts
                    .get(&address)
                    .and_then(|info| self.cache.contracts.get(&info.code_hash).cloned())
            })
            .unwrap_or_else(|| self.db.get_account_code(address))
    }

    #[inline]
    fn get_storage(&self, address: Address, key: Word) -> Word {
        self.cache
            .storage
            .get(&(address, key))
            .copied()
            .unwrap_or_else(|| self.db.get_storage(address, key))
    }

    #[inline]
    fn get_block_hash(&self, number: Word) -> Option<B256> {
        self.cache.block_hashes.get(&number).copied().or_else(|| self.db.get_block_hash(number))
    }
}
