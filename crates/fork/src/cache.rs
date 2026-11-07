//! Shared cache of remote chain state.

use alloy_primitives::{
    Address, B256, U256,
    map::{AddressMap, B256Map, U256Map},
};
use evm2::{bytecode::Bytecode, evm::AccountInfo, interpreter::Word};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

/// Identifies the remote state a [`BlockchainDb`] caches.
///
/// A cache file is only reused when its metadata matches, so state from another chain or block
/// never leaks into a fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockchainDbMeta {
    /// Chain ID of the remote chain.
    pub chain_id: u64,
    /// Block number the state was read at.
    pub block_number: u64,
}

impl BlockchainDbMeta {
    /// Creates metadata for state read at `block_number` on `chain_id`.
    #[inline]
    pub const fn new(chain_id: u64, block_number: u64) -> Self {
        Self { chain_id, block_number }
    }
}

/// Serialized form of a [`BlockchainDb`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonBlockCacheData {
    /// Metadata of the cached state, `None` for data without known provenance.
    pub meta: Option<BlockchainDbMeta>,
    /// Accounts by address; `None` for accounts known to be empty.
    pub accounts: AddressMap<Option<AccountInfo>>,
    /// Storage slots by address, then slot.
    pub storage: AddressMap<U256Map<Word>>,
    /// Block hashes by block number.
    pub block_hashes: U256Map<B256>,
}

/// In-memory state shared by every handle.
#[derive(Debug, Default)]
struct MemDb {
    accounts: RwLock<AddressMap<Option<AccountInfo>>>,
    storage: RwLock<AddressMap<U256Map<Word>>>,
    block_hashes: RwLock<U256Map<B256>>,
    contracts: RwLock<B256Map<Bytecode>>,
}

#[derive(Debug)]
struct Inner {
    meta: Option<BlockchainDbMeta>,
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
        JsonBlockCacheData {
            meta: self.meta,
            accounts: read(&self.db.accounts).clone(),
            storage: read(&self.db.storage).clone(),
            block_hashes: read(&self.db.block_hashes).clone(),
        }
    }
}

/// Remote chain state cached in memory and shared by every clone, optionally backed by a JSON
/// file.
///
/// Like Foundry's `BlockchainDb`, this cache holds only state read from the remote node: writes
/// made during execution belong in a local overlay such as [`ForkDB`](crate::ForkDB). When a
/// cache path is set, the cache is written there once the last clone is dropped, or on
/// [`Self::flush`].
#[derive(Clone, Debug)]
pub struct BlockchainDb {
    inner: Arc<Inner>,
}

impl BlockchainDb {
    /// Creates a cache for `meta`, loading `cache_path` when it holds data for the same chain and
    /// block.
    ///
    /// A missing, unreadable or mismatched cache file starts an empty cache, which replaces the
    /// file on flush.
    pub fn new(meta: BlockchainDbMeta, cache_path: Option<PathBuf>) -> Self {
        let data = cache_path
            .as_deref()
            .and_then(|path| match read_json(path) {
                Ok(data) if data.meta == Some(meta) => Some(data),
                Ok(data) => {
                    tracing::debug!(target: "evm2::fork", path = %path.display(), cached = ?data.meta, ?meta, "ignoring fork cache for another block");
                    None
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => {
                    tracing::warn!(target: "evm2::fork", ?err, path = %path.display(), "failed to read fork cache");
                    None
                }
            })
            .unwrap_or_else(|| JsonBlockCacheData { meta: Some(meta), ..Default::default() });
        Self::from_data(data, cache_path)
    }

    /// Creates an in-memory cache without metadata or a cache file.
    #[inline]
    pub fn in_memory() -> Self {
        Self::from_data(JsonBlockCacheData::default(), None)
    }

    /// Creates a cache from serialized data, flushing to `cache_path` when set.
    pub fn from_data(data: JsonBlockCacheData, cache_path: Option<PathBuf>) -> Self {
        let JsonBlockCacheData { meta, accounts, storage, block_hashes } = data;
        let contracts = accounts
            .values()
            .flatten()
            .filter_map(|info| Some((info.code_hash, info.code.clone()?)))
            .collect();
        let db = MemDb {
            accounts: RwLock::new(accounts),
            storage: RwLock::new(storage),
            block_hashes: RwLock::new(block_hashes),
            contracts: RwLock::new(contracts),
        };
        Self { inner: Arc::new(Inner { meta, cache_path, db }) }
    }

    /// Returns the metadata of the cached state.
    #[inline]
    pub fn meta(&self) -> Option<BlockchainDbMeta> {
        self.inner.meta
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

    /// Returns a cached storage slot.
    pub fn storage(&self, address: &Address, key: &Word) -> Option<Word> {
        read(&self.inner.db.storage).get(address)?.get(key).copied()
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

    /// Clears every cached entry.
    pub fn clear(&self) {
        write(&self.inner.db.accounts).clear();
        write(&self.inner.db.storage).clear();
        write(&self.inner.db.block_hashes).clear();
        write(&self.inner.db.contracts).clear();
    }

    /// Returns a serializable copy of the cached state.
    pub fn snapshot(&self) -> JsonBlockCacheData {
        self.inner.snapshot()
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
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

fn read_json(path: &Path) -> io::Result<JsonBlockCacheData> {
    let file = fs::File::open(path)?;
    Ok(serde_json::from_reader(io::BufReader::new(file))?)
}

/// Writes through a temporary sibling file so a crash never leaves a truncated cache behind.
fn write_json(path: &Path, data: &JsonBlockCacheData) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut writer = BufWriter::new(fs::File::create(&tmp)?);
    serde_json::to_writer(&mut writer, data)?;
    writer.flush()?;
    drop(writer);
    fs::rename(tmp, path)
}
