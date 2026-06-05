//! In-memory cache database.

use super::{DbErrorCode, DbResult, DynDatabase, EmptyDB};
use crate::{
    bytecode::Bytecode,
    evm::state::{
        AccountChangeRef, AccountInfo, StateChangeSink, StateChangeSource, StorageChangeRef,
    },
    interpreter::Word,
};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY,
    map::{AddressMap, B256Map, U256Map, hash_map::Entry},
};
use core::convert::Infallible;

/// A database implementation that stores initial state in memory.
pub type InMemoryDB = CacheDB<EmptyDB>;

/// Cached storage for one account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountStorageCache {
    /// Cached persistent slots for this account.
    pub slots: U256Map<Word>,
    /// Whether missing slots are known to be zero because storage was wiped.
    pub wiped: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl AccountStorageCache {
    /// Marks all storage for this account as wiped.
    #[inline]
    pub fn wipe(&mut self) {
        self.slots.clear();
        self.wiped = true;
    }
}

/// Cache used by [`CacheDB`].
///
/// Accounts and code are stored separately: accounts carry the code hash, and bytecode is keyed by
/// that hash in [`Self::contracts`]. Account and storage entries are authoritative for this cache
/// layer: a cached `None` account or wiped per-account storage cache shadows the wrapped database
/// instead of falling through to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cache {
    /// Accounts keyed by address. `None` means the account is known to be absent/deleted.
    pub accounts: AddressMap<Option<AccountInfo>>,
    /// Contracts keyed by code hash.
    pub contracts: B256Map<Bytecode>,
    /// Persistent storage keyed by account, then slot.
    pub storage: AddressMap<AccountStorageCache>,
    /// Cached block hashes keyed by block number.
    pub block_hashes: U256Map<B256>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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
            storage: AddressMap::default(),
            block_hashes: U256Map::default(),
            _non_exhaustive: (),
        }
    }
}

/// A cache database over another backing database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheDB<ExtDB = EmptyDB> {
    /// The cache that stores all local state.
    pub cache: Cache,
    /// Wrapped backing database.
    pub db: ExtDB,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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
        Self { cache: Cache::default(), db, _non_exhaustive: () }
    }

    /// Applies borrowed state changes to this cache.
    #[inline]
    pub fn commit_source<S: StateChangeSource>(&mut self, source: &S) {
        match source.visit(self) {
            Ok(()) => {}
            Err(err) => match err {},
        }
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
    pub fn insert_account_info(&mut self, address: &Address, mut info: AccountInfo) {
        self.insert_contract(&mut info);
        info.code = None;
        self.cache.accounts.insert(*address, Some(info));
    }

    /// Returns cached account info if the account exists in the cache.
    #[inline]
    pub fn account_info(&self, address: &Address) -> Option<&AccountInfo> {
        self.cache.accounts.get(address).and_then(Option::as_ref)
    }

    /// Returns whether the account is known to be absent from the cache layer.
    #[inline]
    pub(crate) fn account_absent(&self, address: &Address) -> bool {
        self.cache.accounts.get(address).is_some_and(Option::is_none)
    }

    /// Returns a cached storage value if it is known without loading the wrapped database.
    #[inline]
    pub(crate) fn storage_ref(&self, address: &Address, key: &Word) -> Option<Word> {
        self.cache.storage.get(address).and_then(|storage| {
            storage.slots.get(key).copied().or_else(|| storage.wiped.then_some(Word::ZERO))
        })
    }

    /// Inserts persistent storage.
    #[inline]
    pub fn insert_account_storage(&mut self, address: &Address, key: &Word, value: &Word) {
        self.cache.accounts.entry(*address).or_insert_with(|| Some(AccountInfo::default()));
        self.cache.storage.entry(*address).or_default().slots.insert(*key, *value);
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: &Word, hash: &B256) {
        self.cache.block_hashes.insert(*number, *hash);
    }
}

impl<ExtDB> StateChangeSink for CacheDB<ExtDB> {
    type Error = Infallible;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.cache.contracts.insert(code_hash, code.clone());
        Ok(())
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        self.cache.storage.entry(address).or_default().wipe();
        Ok(())
    }

    #[inline]
    fn storage(&mut self, change: StorageChangeRef) -> Result<(), Self::Error> {
        let storage = self.cache.storage.entry(change.address).or_default();
        if change.after_wipe && change.current.is_zero() {
            storage.slots.remove(&change.key);
        } else {
            storage.slots.insert(change.key, change.current);
        }
        Ok(())
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        match change.current {
            Some(info) => self.insert_account_info(&change.address, info.to_account_info()),
            None => {
                self.cache.accounts.insert(change.address, None);
                self.cache.storage.entry(change.address).or_default().wipe();
            }
        }
        Ok(())
    }
}

impl<ExtDB: DynDatabase> DynDatabase for CacheDB<ExtDB> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let Cache { accounts, contracts, .. } = &mut self.cache;
        match accounts.entry(*address) {
            Entry::Occupied(entry) => Ok(entry.get().clone()),
            Entry::Vacant(entry) => {
                let Some(mut info) = self.db.get_account(address)? else {
                    return Ok(entry.insert(None).clone());
                };
                Self::insert_contract_inner(contracts, &mut info);
                info.code = None;
                Ok(entry.insert(Some(info)).clone())
            }
        }
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        match self.cache.contracts.entry(*code_hash) {
            Entry::Occupied(entry) => Ok(entry.get().clone()),
            Entry::Vacant(entry) => Ok(entry.insert(self.db.get_code_by_hash(code_hash)?).clone()),
        }
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        if self.account_absent(address) {
            return Ok(Word::ZERO);
        }

        match self.cache.storage.entry(*address) {
            Entry::Occupied(mut entry) => {
                let storage = entry.get_mut();
                match storage.slots.entry(*key) {
                    Entry::Occupied(slot) => Ok(*slot.get()),
                    Entry::Vacant(slot) => {
                        if storage.wiped {
                            return Ok(Word::ZERO);
                        }
                        let value = self.db.get_storage(address, key)?;
                        Ok(*slot.insert(value))
                    }
                }
            }
            Entry::Vacant(entry) => {
                let value = self.db.get_storage(address, key)?;
                let mut storage = AccountStorageCache::default();
                storage.slots.insert(*key, value);
                entry.insert(storage);
                Ok(value)
            }
        }
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        match self.cache.block_hashes.entry(*number) {
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

    impl crate::evm::Database for CountingDB {
        type Error = core::convert::Infallible;

        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            self.account_loads += 1;
            Ok(self.account.clone())
        }

        fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
            self.code_loads += 1;
            Ok(self
                .account
                .as_ref()
                .filter(|info| info.code_hash == *code_hash)
                .and_then(|info| info.code.clone())
                .unwrap_or_default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            self.storage_loads += 1;
            Ok(self.storage)
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<Option<B256>, Self::Error> {
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
        let mut cache = CacheDB::new(crate::evm::Db::new(db));

        let code_hash = code.hash_slow();
        assert_eq!(
            cache.get_account(&address).unwrap().map(|info| info.code_hash),
            Some(code_hash)
        );
        assert_eq!(cache.get_code_by_hash(&code_hash).unwrap(), code);
        assert_eq!(cache.get_code_by_hash(&code_hash).unwrap(), code);
        assert_eq!(cache.db.inner().account_loads, 1);
        assert_eq!(cache.db.inner().code_loads, 0);

        assert_eq!(cache.get_storage(&address, &key).unwrap(), Word::from(4));
        assert_eq!(cache.get_storage(&address, &key).unwrap(), Word::from(4));
        assert_eq!(cache.db.inner().storage_loads, 1);

        assert_eq!(cache.get_block_hash(&Word::from(5)).unwrap(), Some(block_hash));
        assert_eq!(cache.get_block_hash(&Word::from(5)).unwrap(), Some(block_hash));
        assert_eq!(cache.db.inner().block_hash_loads, 1);
    }
}
