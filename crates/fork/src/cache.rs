//! Shared cache of remote chain state.

use crate::AccountFetchPolicy;
use alloy_primitives::{
    Address, B256, U256,
    map::{AddressMap, AddressSet, B256Map, HashSet, U256Map},
};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountChangeRef, AccountInfo, StateChangeSink, StorageChange},
    interpreter::Word,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    convert::Infallible,
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicBool, Ordering},
    },
};

/// Identifies the remote state a [`BlockchainDb`] caches.
///
/// A cache file is only reused when its metadata matches, so state from another chain or block
/// never leaks into a fork. Hosts are recorded but not compared, so the same chain read through
/// another endpoint (such as HTTP and WebSocket URLs of one node) reuses the cache.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BlockchainDbMeta {
    /// Chain ID of the remote chain.
    pub chain_id: u64,
    /// Block number the state was read at.
    pub block_number: u64,
    /// Hash of the block the state was read at, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_hash: Option<B256>,
    /// Hosts the state was read from.
    #[serde(default)]
    pub hosts: BTreeSet<String>,
    /// How accounts are fetched. Not part of the cache identity.
    #[serde(skip)]
    pub account_fetch_policy: AccountFetchPolicy,
}

impl PartialEq for BlockchainDbMeta {
    fn eq(&self, other: &Self) -> bool {
        (self.chain_id, self.block_number, self.block_hash)
            == (other.chain_id, other.block_number, other.block_hash)
    }
}

impl Eq for BlockchainDbMeta {}

impl BlockchainDbMeta {
    /// Creates metadata for state read at `block_number` on `chain_id`.
    #[inline]
    pub fn new(chain_id: u64, block_number: u64) -> Self {
        Self { chain_id, block_number, ..Default::default() }
    }

    /// Records the host of `url`.
    pub fn with_url(mut self, url: &str) -> Self {
        self.hosts.insert(host(url));
        self
    }

    /// Sets the hash of the block the state is read at.
    #[inline]
    pub const fn with_block_hash(mut self, block_hash: B256) -> Self {
        self.block_hash = Some(block_hash);
        self
    }

    /// Sets how accounts are fetched.
    #[inline]
    pub const fn with_account_fetch_policy(mut self, policy: AccountFetchPolicy) -> Self {
        self.account_fetch_policy = policy;
        self
    }
}

/// Returns the host of `url`, or `url` itself when it has none.
fn host(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else { return url.to_string() };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if authority.starts_with('[') {
        authority.split_inclusive(']').next().unwrap_or(authority)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    if host.is_empty() { url.to_string() } else { host.to_string() }
}

/// Serialized form of a [`BlockchainDb`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonBlockCacheData {
    /// Metadata of the cached state.
    pub meta: BlockchainDbMeta,
    /// Accounts by address; `None` for accounts known to be empty.
    pub accounts: AddressMap<Option<AccountInfo>>,
    /// Storage slots by address, then slot.
    pub storage: AddressMap<U256Map<Word>>,
    /// Block hashes by block number.
    pub block_hashes: U256Map<B256>,
    /// Accounts whose storage was wiped by a commit: their missing slots read as zero.
    #[serde(default, skip_serializing_if = "AddressSet::is_empty")]
    pub wiped: AddressSet,
}

/// A cache entry read ahead of the EVM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PrefetchKey {
    Account(Address),
    Slot(Address, Word),
}

/// In-memory state shared by every handle.
#[derive(Debug, Default)]
struct MemDb {
    accounts: RwLock<AddressMap<Option<AccountInfo>>>,
    storage: RwLock<AddressMap<U256Map<Word>>>,
    block_hashes: RwLock<U256Map<B256>>,
    contracts: RwLock<B256Map<Bytecode>>,
    wiped: RwLock<AddressSet>,
    /// Prefetched entries nothing has read yet; they are left out of the cache file.
    prefetched: RwLock<HashSet<PrefetchKey>>,
    has_prefetched: AtomicBool,
}

#[derive(Debug)]
struct Inner {
    meta: RwLock<BlockchainDbMeta>,
    cache_path: Option<PathBuf>,
    db: MemDb,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let Some(path) = &self.cache_path else { return };
        if let Err(err) = write_json(path, &self.snapshot()) {
            tracing::warn!(target: "evm2::fork", ?err, path = %path.display(), "failed to flush fork cache");
        }
    }
}

impl Inner {
    fn snapshot(&self) -> JsonBlockCacheData {
        let mut data = JsonBlockCacheData {
            meta: read(&self.meta).clone(),
            accounts: read(&self.db.accounts).clone(),
            storage: read(&self.db.storage).clone(),
            block_hashes: read(&self.db.block_hashes).clone(),
            wiped: read(&self.db.wiped).clone(),
        };
        if self.db.has_prefetched.load(Ordering::Relaxed) {
            for key in read(&self.db.prefetched).iter() {
                match key {
                    PrefetchKey::Account(address) => {
                        data.accounts.remove(address);
                    }
                    PrefetchKey::Slot(address, slot) => {
                        if let Some(slots) = data.storage.get_mut(address) {
                            slots.remove(slot);
                            if slots.is_empty() {
                                data.storage.remove(address);
                            }
                        }
                    }
                }
            }
        }
        data
    }
}

/// Remote chain state cached in memory and shared by every clone, optionally backed by a JSON
/// file.
///
/// Like Foundry's `BlockchainDb`, this cache holds state read from the remote node; writes made
/// during execution belong in a local overlay such as [`ForkDB`](crate::ForkDB), unless they are
/// committed here on purpose through its [`StateChangeSink`] implementation. When a cache path is
/// set, the cache is written there once the last clone is dropped, or on [`Self::flush`].
#[derive(Clone, Debug)]
pub struct BlockchainDb {
    inner: Arc<Inner>,
}

impl BlockchainDb {
    /// Creates a cache for `meta`, loading `cache_path` when it holds data for the same chain and
    /// block.
    ///
    /// A missing, unreadable or mismatched cache file starts an empty cache, which replaces the
    /// file on flush. Hosts recorded in the file are merged into `meta`.
    pub fn new(meta: BlockchainDbMeta, cache_path: Option<PathBuf>) -> Self {
        Self::new_db(meta, cache_path, false)
    }

    /// Creates a cache like [`Self::new`], but uses the cache file whatever its metadata.
    ///
    /// Useful to start offline, without fetching the metadata of the block first.
    pub fn new_skip_check(meta: BlockchainDbMeta, cache_path: Option<PathBuf>) -> Self {
        Self::new_db(meta, cache_path, true)
    }

    fn new_db(mut meta: BlockchainDbMeta, cache_path: Option<PathBuf>, skip_check: bool) -> Self {
        let data = cache_path
            .as_deref()
            .and_then(|path| match read_json(path) {
                Ok(mut data) if skip_check || data.meta == meta => {
                    meta.hosts.extend(std::mem::take(&mut data.meta.hosts));
                    if !skip_check {
                        data.meta = meta.clone();
                    } else {
                        data.meta.hosts = meta.hosts.clone();
                        data.meta.account_fetch_policy = meta.account_fetch_policy;
                    }
                    Some(data)
                }
                Ok(data) => {
                    tracing::warn!(target: "evm2::fork", path = %path.display(), cached = ?data.meta, ?meta, "non-matching block metadata, ignoring fork cache");
                    None
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => {
                    tracing::warn!(target: "evm2::fork", ?err, path = %path.display(), "failed to read fork cache");
                    None
                }
            })
            .unwrap_or_else(|| JsonBlockCacheData { meta, ..Default::default() });
        Self::from_data(data, cache_path)
    }

    /// Creates an in-memory cache with default metadata and no cache file.
    #[inline]
    pub fn in_memory() -> Self {
        Self::from_data(JsonBlockCacheData::default(), None)
    }

    /// Creates a cache from serialized data, flushing to `cache_path` when set.
    pub fn from_data(data: JsonBlockCacheData, cache_path: Option<PathBuf>) -> Self {
        let JsonBlockCacheData { meta, accounts, storage, block_hashes, wiped } = data;
        let contracts = contracts_of(&accounts);
        let db = MemDb {
            accounts: RwLock::new(accounts),
            storage: RwLock::new(storage),
            block_hashes: RwLock::new(block_hashes),
            contracts: RwLock::new(contracts),
            wiped: RwLock::new(wiped),
            ..Default::default()
        };
        Self { inner: Arc::new(Inner { meta: RwLock::new(meta), cache_path, db }) }
    }

    /// Returns the metadata of the cached state.
    pub fn meta(&self) -> BlockchainDbMeta {
        read(&self.inner.meta).clone()
    }

    /// Replaces the metadata of the cached state.
    pub fn set_meta(&self, meta: BlockchainDbMeta) {
        *write(&self.inner.meta) = meta;
    }

    /// Returns the file this cache flushes to.
    #[inline]
    pub fn cache_path(&self) -> Option<&Path> {
        self.inner.cache_path.as_deref()
    }

    /// Returns a cached account: `None` when not cached, `Some(None)` when known to be empty.
    pub fn account(&self, address: &Address) -> Option<Option<AccountInfo>> {
        read(&self.inner.db.accounts).get(address).cloned()
    }

    /// Returns a cached storage slot. Missing slots of wiped accounts read as zero.
    pub fn storage(&self, address: &Address, key: &Word) -> Option<Word> {
        if let Some(value) = read(&self.inner.db.storage).get(address).and_then(|s| s.get(key)) {
            return Some(*value);
        }
        read(&self.inner.db.wiped).contains(address).then_some(Word::ZERO)
    }

    /// Returns a cached block hash.
    pub fn block_hash(&self, number: &U256) -> Option<B256> {
        read(&self.inner.db.block_hashes).get(number).copied()
    }

    /// Returns cached bytecode by code hash.
    pub fn code_by_hash(&self, code_hash: &B256) -> Option<Bytecode> {
        read(&self.inner.db.contracts).get(code_hash).cloned()
    }

    /// Inserts or replaces an account; `None` records a known empty account.
    pub fn insert_account(&self, address: Address, info: Option<AccountInfo>) {
        if let Some(info) = &info
            && let Some(code) = &info.code
        {
            write(&self.inner.db.contracts).insert(info.code_hash, code.clone());
        }
        write(&self.inner.db.accounts).insert(address, info);
    }

    /// Inserts or replaces a storage slot.
    pub fn insert_storage(&self, address: Address, key: Word, value: Word) {
        write(&self.inner.db.storage).entry(address).or_default().insert(key, value);
    }

    /// Inserts or replaces a block hash.
    pub fn insert_block_hash(&self, number: U256, hash: B256) {
        write(&self.inner.db.block_hashes).insert(number, hash);
    }

    /// Inserts or replaces accounts.
    pub fn extend_accounts(&self, accounts: AddressMap<Option<AccountInfo>>) {
        write(&self.inner.db.contracts).extend(contracts_of(&accounts));
        write(&self.inner.db.accounts).extend(accounts);
    }

    /// Inserts or replaces the storage of accounts, replacing each account's cached slots.
    pub fn extend_storage(&self, storage: AddressMap<U256Map<Word>>) {
        write(&self.inner.db.storage).extend(storage);
    }

    /// Inserts or replaces block hashes.
    pub fn extend_block_hashes(&self, block_hashes: U256Map<B256>) {
        write(&self.inner.db.block_hashes).extend(block_hashes);
    }

    /// Returns a copy of every cached account.
    pub fn accounts(&self) -> AddressMap<Option<AccountInfo>> {
        read(&self.inner.db.accounts).clone()
    }

    /// Returns a copy of every cached storage slot.
    pub fn storages(&self) -> AddressMap<U256Map<Word>> {
        read(&self.inner.db.storage).clone()
    }

    /// Returns a copy of every cached block hash.
    pub fn block_hashes(&self) -> U256Map<B256> {
        read(&self.inner.db.block_hashes).clone()
    }

    /// Returns the number of cached accounts.
    pub fn accounts_len(&self) -> usize {
        read(&self.inner.db.accounts).len()
    }

    /// Returns the number of accounts with cached storage.
    pub fn storage_len(&self) -> usize {
        read(&self.inner.db.storage).len()
    }

    /// Returns the number of cached block hashes.
    pub fn block_hashes_len(&self) -> usize {
        read(&self.inner.db.block_hashes).len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.accounts_len() == 0 && self.storage_len() == 0 && self.block_hashes_len() == 0
    }

    /// Clears every cached entry.
    pub fn clear(&self) {
        write(&self.inner.db.accounts).clear();
        write(&self.inner.db.storage).clear();
        write(&self.inner.db.block_hashes).clear();
        write(&self.inner.db.contracts).clear();
        write(&self.inner.db.wiped).clear();
        write(&self.inner.db.prefetched).clear();
        self.inner.db.has_prefetched.store(false, Ordering::Relaxed);
    }

    /// Returns a serializable copy of the cached state, without prefetched entries nothing read.
    pub fn snapshot(&self) -> JsonBlockCacheData {
        self.inner.snapshot()
    }

    /// Replaces the cached state with `data`, keeping this cache's metadata.
    pub fn restore(&self, data: JsonBlockCacheData) {
        let JsonBlockCacheData { meta: _, accounts, storage, block_hashes, wiped } = data;
        self.clear();
        self.extend_accounts(accounts);
        *write(&self.inner.db.storage) = storage;
        *write(&self.inner.db.block_hashes) = block_hashes;
        *write(&self.inner.db.wiped) = wiped;
    }

    /// Writes the cache to its cache path, if any.
    pub fn flush(&self) -> io::Result<()> {
        let Some(path) = &self.inner.cache_path else { return Ok(()) };
        self.flush_to(path)
    }

    /// Writes the cache to `path` as JSON.
    pub fn flush_to(&self, path: &Path) -> io::Result<()> {
        write_json(path, &self.snapshot())
    }

    /// Records that `key` was prefetched and has not been read yet.
    pub(crate) fn mark_prefetched(&self, key: PrefetchKey) {
        write(&self.inner.db.prefetched).insert(key);
        self.inner.db.has_prefetched.store(true, Ordering::Relaxed);
    }

    /// Records that `key` was read, so it is flushed even if it was prefetched.
    #[inline]
    pub(crate) fn note_read(&self, key: PrefetchKey) {
        if self.inner.db.has_prefetched.load(Ordering::Relaxed) {
            write(&self.inner.db.prefetched).remove(&key);
        }
    }

    pub(crate) fn write_accounts(&self) -> RwLockWriteGuard<'_, AddressMap<Option<AccountInfo>>> {
        write(&self.inner.db.accounts)
    }

    pub(crate) fn write_storage(&self) -> RwLockWriteGuard<'_, AddressMap<U256Map<Word>>> {
        write(&self.inner.db.storage)
    }

    pub(crate) fn write_contracts(&self) -> RwLockWriteGuard<'_, B256Map<Bytecode>> {
        write(&self.inner.db.contracts)
    }
}

/// Commits state changes into the shared cache, like Foundry's `MemDb::do_commit`.
///
/// Every handle sees committed state as if it were remote state, and it is flushed to the cache
/// file. Commit into a local overlay such as [`ForkDB`](crate::ForkDB) instead to keep changes
/// private to one execution.
impl StateChangeSink for BlockchainDb {
    type Error = Infallible;

    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        write(&self.inner.db.contracts).insert(code_hash, code.clone());
        Ok(())
    }

    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        match change.current {
            Some(info) => self.insert_account(change.address, Some(info.clone())),
            None => {
                self.insert_account(change.address, None);
                self.storage_wipe(change.address)?;
            }
        }
        Ok(())
    }

    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        write(&self.inner.db.storage).remove(&address);
        write(&self.inner.db.wiped).insert(address);
        Ok(())
    }

    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.insert_storage(change.address, change.key, change.current);
        Ok(())
    }
}

fn contracts_of(accounts: &AddressMap<Option<AccountInfo>>) -> B256Map<Bytecode> {
    accounts
        .values()
        .flatten()
        .filter_map(|info| Some((info.code_hash, info.code.clone()?)))
        .collect()
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Reads a cache file.
pub(crate) fn read_json(path: &Path) -> io::Result<JsonBlockCacheData> {
    let file = fs::File::open(path)?;
    Ok(serde_json::from_reader(io::BufReader::new(file))?)
}

/// Writes through a temporary sibling file so a crash never leaves a truncated cache behind.
fn write_json(path: &Path, data: &JsonBlockCacheData) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut writer = BufWriter::new(fs::File::create(&tmp)?);
    serde_json::to_writer(&mut writer, data)?;
    writer.flush()?;
    drop(writer);
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_strips_scheme_credentials_port_and_path() {
        assert_eq!(host("https://user:pw@eth.example.com:8545/v1/key?x=1"), "eth.example.com");
        assert_eq!(host("ws://[::1]:8546"), "[::1]");
        assert_eq!(host("/tmp/reth.ipc"), "/tmp/reth.ipc");
    }

    #[test]
    fn metadata_ignores_hosts() {
        let a = BlockchainDbMeta::new(1, 5).with_url("https://a.example");
        let b = BlockchainDbMeta::new(1, 5).with_url("wss://b.example");
        assert_eq!(a, b);
        assert_ne!(a, BlockchainDbMeta::new(1, 6));
        assert_ne!(a, b.with_block_hash(B256::repeat_byte(1)));
    }
}
