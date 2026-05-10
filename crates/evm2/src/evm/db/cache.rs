//! In-memory cache database.

use super::{Database, DatabaseCommit, DbErrorCode, DbResult, EmptyDB};
use crate::{
    bytecode::Bytecode,
    evm::state::{AccountInfo, StateChanges},
    interpreter::Word,
    storage_key::{StorageKey, StorageKeyMap},
};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY,
    map::{AddressMap, B256Map, U256Map, hash_map::Entry},
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
    pub storage: StorageKeyMap<Word>,
    /// Cached block hashes keyed by block number.
    pub block_hashes: U256Map<B256>,
}

impl Default for Cache {
    #[inline]
    fn default() -> Self {
        let mut contracts = B256Map::default();
        contracts.insert(KECCAK256_EMPTY, Bytecode::default());
        contracts.insert(B256::ZERO, Bytecode::default());
        Self {
            accounts: AddressMap::default(),
            contracts,
            storage: StorageKeyMap::default(),
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
    pub fn insert_contract(&mut self, info: &mut AccountInfo) {
        Self::insert_contract_inner(&mut self.cache.contracts, info);
    }

    #[inline]
    fn insert_contract_inner(contracts: &mut B256Map<Bytecode>, info: &mut AccountInfo) {
        if let Some(code) = &info.code
            && !code.is_empty()
        {
            if info.code_hash == KECCAK256_EMPTY {
                info.code_hash = code.hash_slow();
            }
            contracts.entry(info.code_hash).or_insert_with(|| code.clone());
        }
        if info.code_hash.is_zero() {
            info.code_hash = KECCAK256_EMPTY;
        }
    }

    /// Inserts account info.
    #[inline]
    pub fn insert_account_info(&mut self, address: Address, mut info: AccountInfo) {
        self.insert_contract(&mut info);
        info.code = None;
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
        self.cache.storage.insert(StorageKey::new(address, key), value);
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: Word, hash: B256) {
        self.cache.block_hashes.insert(number, hash);
    }
}

impl<ExtDB> DatabaseCommit for CacheDB<ExtDB> {
    fn commit(&mut self, changes: &StateChanges) {
        for (&code_hash, code) in &changes.code {
            self.cache.contracts.insert(code_hash, code.clone());
        }
        for (&address, storage) in &changes.storage {
            if storage.wipe {
                self.cache.storage.retain(|key, _| key.address() != address);
            }
            for (&key, change) in &storage.slots {
                if change.current.is_zero() {
                    self.cache.storage.remove(&StorageKey::new(address, key));
                } else {
                    self.cache.storage.insert(StorageKey::new(address, key), change.current);
                }
            }
        }
        for (&address, change) in &changes.accounts {
            match &change.current {
                Some(info) => self.insert_account_info(address, info.clone()),
                None => {
                    self.cache.accounts.remove(&address);
                    self.cache.storage.retain(|key, _| key.address() != address);
                }
            }
        }
    }
}

impl<ExtDB: Database> Database for CacheDB<ExtDB> {
    #[inline]
    fn get_account(&mut self, address: Address) -> DbResult<Option<AccountInfo>> {
        let Cache { accounts, contracts, .. } = &mut self.cache;
        match accounts.entry(address) {
            Entry::Occupied(entry) => Ok(Some(entry.get().clone())),
            Entry::Vacant(entry) => {
                let Some(mut info) = self.db.get_account(address)? else {
                    return Ok(None);
                };
                Self::insert_contract_inner(contracts, &mut info);
                info.code = None;
                Ok(Some(entry.insert(info).clone()))
            }
        }
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: B256) -> DbResult<Bytecode> {
        match self.cache.contracts.entry(code_hash) {
            Entry::Occupied(entry) => Ok(entry.get().clone()),
            Entry::Vacant(entry) => Ok(entry.insert(self.db.get_code_by_hash(code_hash)?).clone()),
        }
    }

    #[inline]
    fn get_storage(&mut self, address: Address, key: Word) -> DbResult<Word> {
        match self.cache.storage.entry(StorageKey::new(address, key)) {
            Entry::Occupied(entry) => Ok(*entry.get()),
            Entry::Vacant(entry) => {
                let value = self.db.get_storage(address, key)?;
                Ok(*entry.insert(value))
            }
        }
    }

    #[inline]
    fn get_block_hash(&mut self, number: Word) -> DbResult<Option<B256>> {
        match self.cache.block_hashes.entry(number) {
            Entry::Occupied(entry) => Ok(Some(*entry.get())),
            Entry::Vacant(entry) => {
                let Some(hash) = self.db.get_block_hash(number)? else {
                    return Ok(None);
                };
                Ok(Some(*entry.insert(hash)))
            }
        }
    }

    #[inline]
    fn error(&mut self, code: DbErrorCode) -> alloc::boxed::Box<dyn core::error::Error> {
        self.db.error(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::op;
    use alloy_primitives::Bytes;

    #[derive(Debug, Default)]
    struct CountingDB {
        account: Option<AccountInfo>,
        storage: Word,
        block_hash: Option<B256>,
        account_loads: usize,
        code_loads: usize,
        storage_loads: usize,
        block_hash_loads: usize,
    }

    impl Database for CountingDB {
        fn get_account(&mut self, _address: Address) -> DbResult<Option<AccountInfo>> {
            self.account_loads += 1;
            Ok(self.account.clone())
        }

        fn get_code_by_hash(&mut self, code_hash: B256) -> DbResult<Bytecode> {
            self.code_loads += 1;
            Ok(self
                .account
                .as_ref()
                .filter(|info| info.code_hash == code_hash)
                .and_then(|info| info.code.clone())
                .unwrap_or_default())
        }

        fn get_storage(&mut self, _address: Address, _key: Word) -> DbResult<Word> {
            self.storage_loads += 1;
            Ok(self.storage)
        }

        fn get_block_hash(&mut self, _number: Word) -> DbResult<Option<B256>> {
            self.block_hash_loads += 1;
            Ok(self.block_hash)
        }
    }

    #[test]
    fn cache_db_caches_wrapped_db_reads() {
        let address = Address::with_last_byte(1);
        let key = Word::from(2);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let block_hash = B256::with_last_byte(3);
        let db = CountingDB {
            account: Some(AccountInfo::default().with_code(code.clone())),
            storage: Word::from(4),
            block_hash: Some(block_hash),
            ..CountingDB::default()
        };
        let mut cache = CacheDB::new(db);

        let code_hash = code.hash_slow();
        assert_eq!(cache.get_account(address).unwrap().map(|info| info.code_hash), Some(code_hash));
        assert_eq!(cache.get_code_by_hash(code_hash).unwrap(), code);
        assert_eq!(cache.get_code_by_hash(code_hash).unwrap(), code);
        assert_eq!(cache.db.account_loads, 1);
        assert_eq!(cache.db.code_loads, 0);

        assert_eq!(cache.get_storage(address, key).unwrap(), Word::from(4));
        assert_eq!(cache.get_storage(address, key).unwrap(), Word::from(4));
        assert_eq!(cache.db.storage_loads, 1);

        assert_eq!(cache.get_block_hash(Word::from(5)).unwrap(), Some(block_hash));
        assert_eq!(cache.get_block_hash(Word::from(5)).unwrap(), Some(block_hash));
        assert_eq!(cache.db.block_hash_loads, 1);
    }
}
