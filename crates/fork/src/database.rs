//! A forked database with state snapshots, modeled on Foundry's `ForkedDatabase`.

use crate::{BlockchainDb, ForkDB, ForkError, ForkResult, JsonBlockCacheData, SharedBackend};
use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, KECCAK256_EMPTY, U256, map::HashMap};
use alloy_provider::{
    Network,
    network::{BlockResponse, Ethereum, primitives::HeaderResponse},
};
use evm2::{
    bytecode::Bytecode,
    evm::{
        AccountChangeRef, AccountInfo, Cache, CacheDB, DatabaseRef, DbRef, DbResult, DynDatabase,
        PendingState, StateChangeSink, StateChangeSource, StorageChange,
    },
    interpreter::Word,
};
use std::{
    convert::Infallible,
    fmt, io,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// State snapshots by ID, like Foundry's `StateSnapshots`.
///
/// IDs count up from zero. Removing a snapshot with [`Self::remove`] also drops every snapshot
/// taken after it, so reverting to an earlier state invalidates later snapshots.
#[derive(Clone, Debug)]
pub struct StateSnapshots<T> {
    id: U256,
    state_snapshots: HashMap<U256, T>,
}

impl<T> Default for StateSnapshots<T> {
    fn default() -> Self {
        Self { id: U256::ZERO, state_snapshots: HashMap::default() }
    }
}

impl<T> StateSnapshots<T> {
    /// Returns the snapshot with ID `id`.
    pub fn get(&self, id: U256) -> Option<&T> {
        self.state_snapshots.get(&id)
    }

    /// Whether there are no snapshots.
    pub fn is_empty(&self) -> bool {
        self.state_snapshots.is_empty()
    }

    /// Removes the snapshot with ID `id` and every snapshot taken after it.
    ///
    /// Reverting to ID 1 removes snapshots 1, 2, 3 and so on. Snapshots inserted with
    /// [`Self::insert_at`] at IDs not handed out yet are kept.
    pub fn remove(&mut self, id: U256) -> Option<T> {
        let state_snapshot = self.state_snapshots.remove(&id);
        let next = self.id;
        self.state_snapshots.retain(|&other, _| other <= id || other >= next);
        state_snapshot
    }

    /// Removes the snapshot with ID `id`, keeping the snapshots taken after it.
    pub fn remove_at(&mut self, id: U256) -> Option<T> {
        self.state_snapshots.remove(&id)
    }

    /// Removes every snapshot. IDs keep counting from where they were.
    pub fn clear(&mut self) {
        self.state_snapshots.clear();
    }

    /// Inserts a snapshot and returns its ID.
    pub fn insert(&mut self, state_snapshot: T) -> U256 {
        let id = self.id;
        self.id = id.saturating_add(U256::ONE);
        self.state_snapshots.insert(id, state_snapshot);
        id
    }

    /// Inserts a snapshot at ID `id`, replacing any snapshot there.
    ///
    /// Does not advance the next ID.
    pub fn insert_at(&mut self, state_snapshot: T, id: U256) {
        self.state_snapshots.insert(id, state_snapshot);
    }
}

/// What happens to a snapshot once it is reverted to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RevertStateSnapshotAction {
    /// Keep the snapshot, so it can be reverted to again.
    #[default]
    RevertKeep,
    /// Remove the snapshot.
    RevertRemove,
}

impl RevertStateSnapshotAction {
    /// Whether the snapshot is kept after the revert.
    #[inline]
    pub const fn is_keep(&self) -> bool {
        matches!(self, Self::RevertKeep)
    }
}

/// A snapshot of a [`ForkedDatabase`]: its local overlay and the shared remote cache.
///
/// Reads through its [`DatabaseRef`] implementation see the snapshotted state: the local overlay
/// first, then the snapshotted remote cache, then the backend for anything neither holds. Reads
/// never cache anything.
pub struct ForkDbStateSnapshot<N: Network = Ethereum> {
    /// The local overlay, holding every committed change.
    pub local: ForkDB<N>,
    /// The remote state cached when the snapshot was taken.
    pub remote: JsonBlockCacheData,
}

impl<N: Network> Clone for ForkDbStateSnapshot<N> {
    fn clone(&self) -> Self {
        Self { local: self.local.clone(), remote: self.remote.clone() }
    }
}

impl<N: Network> fmt::Debug for ForkDbStateSnapshot<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForkDbStateSnapshot")
            .field("local", &self.local)
            .field("remote", &self.remote)
            .finish()
    }
}

impl<N: Network> ForkDbStateSnapshot<N> {
    const fn reader(&self) -> OverlayReader<'_, N> {
        OverlayReader {
            local: &self.local.cache,
            remote: Some(&self.remote),
            backend: self.local.db.inner(),
        }
    }
}

impl<N: Network> DatabaseRef for ForkDbStateSnapshot<N> {
    type Error = ForkError;

    fn get_account(&self, address: &Address) -> ForkResult<Option<AccountInfo>> {
        self.reader().get_account(address)
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> ForkResult<Bytecode> {
        self.reader().get_code_by_hash(code_hash)
    }

    fn get_storage(&self, address: &Address, key: &Word) -> ForkResult<Word> {
        self.reader().get_storage(address, key)
    }

    fn get_block_hash(&self, number: &Word) -> ForkResult<B256> {
        self.reader().get_block_hash(number)
    }
}

/// A database forked off a remote node, with a local overlay and state snapshots.
///
/// Missing state is fetched through the [`SharedBackend`] into the shared [`BlockchainDb`],
/// which only ever holds remote state; committed changes stay in the local [`ForkDB`] overlay.
/// [`Self::insert_state_snapshot`] records both, and [`Self::revert_state_snapshot`] restores
/// them. Clones share the backend, the remote cache and the snapshots, but each has its own
/// overlay.
///
/// Commit execution results through its [`StateChangeSink`] implementation,
/// [`Self::commit_source`] or [`Self::commit_pending`].
pub struct ForkedDatabase<N: Network = Ethereum> {
    backend: SharedBackend<N>,
    cache_db: ForkDB<N>,
    state_snapshots: Arc<Mutex<StateSnapshots<ForkDbStateSnapshot<N>>>>,
}

impl<N: Network> Clone for ForkedDatabase<N> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            cache_db: self.cache_db.clone(),
            state_snapshots: Arc::clone(&self.state_snapshots),
        }
    }
}

impl<N: Network> fmt::Debug for ForkedDatabase<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForkedDatabase")
            .field("backend", &self.backend)
            .field("cache_db", &self.cache_db)
            .field("state_snapshots", &self.state_snapshots)
            .finish()
    }
}

impl<N: Network> ForkedDatabase<N> {
    /// Creates a database with an empty overlay over `backend`.
    pub fn new(backend: SharedBackend<N>) -> Self {
        Self {
            cache_db: CacheDB::new(DbRef::new(backend.clone())),
            backend,
            state_snapshots: Arc::default(),
        }
    }

    /// Returns the local overlay.
    #[inline]
    pub const fn database(&self) -> &ForkDB<N> {
        &self.cache_db
    }

    /// Returns the local overlay mutably.
    #[inline]
    pub const fn database_mut(&mut self) -> &mut ForkDB<N> {
        &mut self.cache_db
    }

    /// Returns the backend that fetches remote state.
    #[inline]
    pub const fn backend(&self) -> &SharedBackend<N> {
        &self.backend
    }

    /// Returns the shared cache of remote state.
    #[inline]
    pub const fn inner(&self) -> &BlockchainDb {
        self.backend.data()
    }

    /// Returns the state snapshots, shared by every clone.
    #[inline]
    pub const fn state_snapshots(&self) -> &Arc<Mutex<StateSnapshots<ForkDbStateSnapshot<N>>>> {
        &self.state_snapshots
    }

    /// Resets the fork to fresh remote state at `block`.
    ///
    /// Pins the backend to `block`, clears the remote cache and replaces the overlay with an empty
    /// one, dropping every committed change. Snapshots are dropped too: they hold the old block's
    /// state, which reverting to would mix with the new pin.
    pub fn reset(&mut self, block: impl Into<BlockId>) -> ForkResult<()> {
        let mut block = block.into();
        // Name the state the cache will hold, so a later flush is not mistaken for the old
        // block's cache. A tag or hash is resolved to its block, and a tag pinned to that block's
        // hash so the state cannot move under the cache.
        let mut meta = self.inner().meta();
        if let BlockId::Number(BlockNumberOrTag::Number(number)) = block {
            meta.block_number = number;
            meta.block_hash = None;
        } else {
            let resolved = self.backend.get_full_block(block)?;
            let header = resolved.header();
            meta.block_number = header.number();
            meta.block_hash = Some(header.hash());
            block = BlockId::hash(header.hash());
        }
        self.backend.repin(block)?;
        self.inner().set_meta(meta);
        self.cache_db = CacheDB::new(DbRef::new(self.backend.clone()));
        self.lock_state_snapshots().clear();
        tracing::trace!(target: "evm2::fork", "reset forked database");
        Ok(())
    }

    /// Writes the remote cache to its cache path, if any.
    pub fn flush_cache(&self) -> io::Result<()> {
        self.backend.flush_cache()
    }

    /// Returns a snapshot of the overlay and the remote cache.
    pub fn create_state_snapshot(&self) -> ForkDbStateSnapshot<N> {
        ForkDbStateSnapshot { local: self.cache_db.clone(), remote: self.inner().snapshot() }
    }

    /// Takes a snapshot and returns its ID.
    pub fn insert_state_snapshot(&self) -> U256 {
        let state_snapshot = self.create_state_snapshot();
        let id = self.lock_state_snapshots().insert(state_snapshot);
        tracing::trace!(target: "evm2::fork", %id, "created state snapshot");
        id
    }

    /// Restores the overlay and the remote cache from snapshot `id`.
    ///
    /// Snapshots taken after `id` are removed; `id` itself is kept when `action` says so. Returns
    /// `false` when there is no snapshot `id`.
    pub fn revert_state_snapshot(&mut self, id: U256, action: RevertStateSnapshotAction) -> bool {
        let state_snapshot = {
            let mut state_snapshots = self.lock_state_snapshots();
            let state_snapshot = state_snapshots.remove(id);
            if let Some(state_snapshot) = &state_snapshot
                && action.is_keep()
            {
                state_snapshots.insert_at(state_snapshot.clone(), id);
            }
            state_snapshot
        };
        let Some(ForkDbStateSnapshot { local, remote }) = state_snapshot else {
            tracing::warn!(target: "evm2::fork", %id, "no state snapshot to revert to");
            return false;
        };
        self.inner().restore(remote);
        self.cache_db = local;
        tracing::trace!(target: "evm2::fork", %id, "reverted state snapshot");
        true
    }

    /// Applies borrowed state changes to the overlay, like [`CacheDB::commit_source`].
    #[inline]
    pub fn commit_source<S: StateChangeSource>(&mut self, source: &S) {
        self.cache_db.commit_source(source);
    }

    /// Applies a committed transaction's post-state to the overlay, like
    /// [`CacheDB::commit_pending`].
    #[inline]
    pub fn commit_pending(&mut self, pending: &PendingState) {
        self.cache_db.commit_pending(pending);
    }

    fn lock_state_snapshots(&self) -> MutexGuard<'_, StateSnapshots<ForkDbStateSnapshot<N>>> {
        self.state_snapshots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    const fn reader(&self) -> OverlayReader<'_, N> {
        OverlayReader {
            local: &self.cache_db.cache,
            remote: None,
            backend: self.cache_db.db.inner(),
        }
    }
}

/// Reads through the overlay, which caches whatever the backend returns.
impl<N: Network> DynDatabase for ForkedDatabase<N> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.cache_db.get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.cache_db.get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.cache_db.get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        self.cache_db.get_block_hash(number)
    }
}

/// Reads the overlay's cached state, then the backend, without caching anything in the overlay.
///
/// Unlike the [`DynDatabase`] reads, these ignore a block access list attached to the overlay.
impl<N: Network> DatabaseRef for ForkedDatabase<N> {
    type Error = ForkError;

    fn get_account(&self, address: &Address) -> ForkResult<Option<AccountInfo>> {
        self.reader().get_account(address)
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> ForkResult<Bytecode> {
        self.reader().get_code_by_hash(code_hash)
    }

    fn get_storage(&self, address: &Address, key: &Word) -> ForkResult<Word> {
        self.reader().get_storage(address, key)
    }

    fn get_block_hash(&self, number: &Word) -> ForkResult<B256> {
        self.reader().get_block_hash(number)
    }
}

/// Commits state changes into the local overlay; the remote cache is never written.
impl<N: Network> StateChangeSink for ForkedDatabase<N> {
    type Error = Infallible;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.cache_db.bytecode(code_hash, code)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.cache_db.account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        self.cache_db.storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.cache_db.storage(change)
    }

    #[inline]
    fn account_read(
        &mut self,
        address: Address,
        info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        self.cache_db.account_read(address, info)
    }

    #[inline]
    fn storage_read(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
    ) -> Result<(), Self::Error> {
        self.cache_db.storage_read(address, key, value)
    }
}

/// Reads a [`CacheDB`] cache without writing to it, then optional snapshotted remote state, then
/// the backend.
struct OverlayReader<'a, N: Network> {
    local: &'a Cache,
    remote: Option<&'a JsonBlockCacheData>,
    backend: &'a SharedBackend<N>,
}

impl<N: Network> OverlayReader<'_, N> {
    fn get_account(&self, address: &Address) -> ForkResult<Option<AccountInfo>> {
        if let Some(account) = self.local.accounts.get(address) {
            return Ok(account.clone().map(|mut info| {
                // The cache fields are public, so entries written directly can carry a zero hash.
                if info.code_hash.is_zero() {
                    info.code_hash = KECCAK256_EMPTY;
                }
                info
            }));
        }
        if let Some(remote) = self.remote
            && let Some(account) = remote.accounts.get(address)
        {
            return Ok(account.clone());
        }
        self.backend.get_account(address)
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> ForkResult<Bytecode> {
        let code_hash = if code_hash.is_zero() { &KECCAK256_EMPTY } else { code_hash };
        if let Some(code) = self.local.contracts.get(code_hash) {
            return Ok(code.clone());
        }
        if let Some(remote) = self.remote
            && let Some(code) = remote
                .accounts
                .values()
                .flatten()
                .find(|info| info.code_hash == *code_hash)
                .and_then(|info| info.code.clone())
        {
            return Ok(code);
        }
        self.backend.get_code_by_hash(code_hash)
    }

    fn get_storage(&self, address: &Address, key: &Word) -> ForkResult<Word> {
        if let Some(storage) = self.local.storage.get(address) {
            if let Some(value) = storage.slots.get(key) {
                return Ok(*value);
            }
            if storage.wiped {
                return Ok(Word::ZERO);
            }
        }
        // A locally deleted account has no storage below the overlay.
        if self.local.accounts.get(address).is_some_and(Option::is_none) {
            return Ok(Word::ZERO);
        }
        if let Some(remote) = self.remote {
            if let Some(value) = remote.storage.get(address).and_then(|slots| slots.get(key)) {
                return Ok(*value);
            }
            if remote.wiped.contains(address) {
                return Ok(Word::ZERO);
            }
        }
        self.backend.get_storage(address, key)
    }

    fn get_block_hash(&self, number: &Word) -> ForkResult<B256> {
        if let Some(hash) = self.local.block_hashes.get(number) {
            return Ok(*hash);
        }
        if let Some(remote) = self.remote
            && let Some(hash) = remote.block_hashes.get(number)
        {
            return Ok(*hash);
        }
        self.backend.get_block_hash(number)
    }
}
