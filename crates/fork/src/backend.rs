//! Background RPC handler and the shared database handle that talks to it.

use crate::{BlockchainDb, ForkError, ForkResult, NON_ARCHIVE_NODE_WARNING, cache::PrefetchKey};
use alloy_eips::BlockId;
use alloy_primitives::{
    Address, B256, Bytes, U256,
    map::{AddressMap, HashMap, U256Map},
};
use alloy_provider::{
    DynProvider, Network, Provider,
    network::{BlockResponse, Ethereum, primitives::HeaderResponse},
    transport::TransportError,
};
use alloy_rpc_types_eth::BlockNumberOrTag;
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, DatabaseRef},
    interpreter::Word,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    error::Error,
    fmt,
    future::Future,
    path::Path,
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Sender},
    },
    thread,
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

type ResultSender<T> = Sender<ForkResult<T>>;
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type RpcResult<T> = Result<T, Arc<TransportError>>;

/// How the handler fetches accounts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AccountFetchPolicy {
    /// Race the non-standard `eth_getAccountInfo` against `eth_getBalance`,
    /// `eth_getTransactionCount` and `eth_getCode` on the first fetch, then keep using whichever
    /// the endpoint supports.
    #[default]
    Auto,
    /// Use `eth_getAccountInfo`, falling back to separate requests if the endpoint rejects it.
    AccountInfo,
    /// Always use separate `eth_getBalance`, `eth_getTransactionCount` and `eth_getCode`
    /// requests.
    SeparateRequests,
}

const FETCH_UNCHECKED: u8 = 0;
const FETCH_ACCOUNT_INFO: u8 = 1;
const FETCH_SEPARATE: u8 = 2;

impl AccountFetchPolicy {
    const fn initial_mode(self) -> u8 {
        match self {
            Self::Auto => FETCH_UNCHECKED,
            Self::AccountInfo => FETCH_ACCOUNT_INFO,
            Self::SeparateRequests => FETCH_SEPARATE,
        }
    }
}

/// How a [`SharedBackend`] blocks while it waits for the handler.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlockingMode {
    /// Use [`tokio::task::block_in_place`] on a multi-threaded Tokio runtime's worker, and block
    /// the thread anywhere else.
    #[default]
    Auto,
    /// Always use [`tokio::task::block_in_place`].
    ///
    /// Panics on a current-thread runtime.
    BlockInPlace,
    /// Block the calling thread, such as on a Tokio blocking task.
    Block,
}

impl BlockingMode {
    /// Runs `f` the way this mode blocks.
    pub fn run<F: FnOnce() -> R, R>(&self, f: F) -> R {
        match self {
            Self::Auto => match Handle::try_current() {
                Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(f)
                }
                _ => f(),
            },
            Self::BlockInPlace => tokio::task::block_in_place(f),
            Self::Block => f(),
        }
    }
}

/// A request from a [`SharedBackend`] to its [`BackendHandler`].
enum BackendRequest<N: Network> {
    Account(Address, ResultSender<Option<AccountInfo>>),
    Storage(Address, Word, ResultSender<Word>),
    BlockHash(u64, ResultSender<B256>),
    FullBlock(BlockId, ResultSender<N::BlockResponse>),
    Transaction(B256, ResultSender<N::TransactionResponse>),
    SetPinnedBlock(BlockId),
    UpdateAccounts(AddressMap<Option<AccountInfo>>),
    UpdateStorage(AddressMap<U256Map<Word>>),
    UpdateBlockHashes(U256Map<B256>),
    /// A future that answers its own sender.
    Any(BoxFuture<()>),
}

/// A finished RPC fetch whose result is cached and fanned out to waiters.
enum ProviderResponse {
    Account(Address, RpcResult<Option<AccountInfo>>),
    Storage(Address, Word, RpcResult<Word>),
    BlockHash(u64, RpcResult<Option<B256>>),
    /// A request that answered its sender itself.
    Done,
}

/// Serves [`SharedBackend`] requests by fetching remote state through a provider.
///
/// Every state request is first checked against the shared [`BlockchainDb`]. Missing entries are
/// fetched at the pinned block, and concurrent requests for the same account, slot or block hash
/// wait on one in-flight fetch. Results are stored in the cache before waiters are answered.
///
/// The handler runs until every [`SharedBackend`] handle is dropped; drive it with
/// [`Self::run`] or let [`SharedBackend::spawn_backend`] or
/// [`SharedBackend::spawn_backend_thread`] do so.
#[must_use = "the handler does nothing unless run"]
pub struct BackendHandler<N: Network = Ethereum> {
    provider: DynProvider<N>,
    db: BlockchainDb,
    incoming: UnboundedReceiver<BackendRequest<N>>,
    block_id: BlockId,
    account_fetch_mode: Arc<AtomicU8>,
    account_requests: AddressMap<Vec<ResultSender<Option<AccountInfo>>>>,
    storage_requests: HashMap<(Address, Word), Vec<ResultSender<Word>>>,
    block_requests: HashMap<u64, Vec<ResultSender<B256>>>,
    pending: FuturesUnordered<BoxFuture<ProviderResponse>>,
}

impl<N: Network> fmt::Debug for BackendHandler<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackendHandler")
            .field("block_id", &self.block_id)
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl<N: Network> BackendHandler<N> {
    /// Processes requests until every [`SharedBackend`] handle is dropped.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                request = self.incoming.recv() => match request {
                    Some(request) => self.on_request(request),
                    // Every handle is gone, so nobody waits on in-flight fetches.
                    None => break,
                },
                Some(response) = self.pending.next(), if !self.pending.is_empty() => {
                    self.on_response(response);
                }
            }
        }
    }

    fn on_request(&mut self, request: BackendRequest<N>) {
        match request {
            BackendRequest::Account(address, sender) => {
                if let Some(info) = self.db.account(&address) {
                    let _ = sender.send(Ok(info));
                    return;
                }
                let waiters = self.account_requests.entry(address).or_default();
                waiters.push(sender);
                if waiters.len() == 1 {
                    let fetch = fetch_account(
                        self.provider.clone(),
                        address,
                        self.block_id,
                        Arc::clone(&self.account_fetch_mode),
                    );
                    self.pending.push(Box::pin(async move {
                        ProviderResponse::Account(address, fetch.await.map_err(Arc::new))
                    }));
                }
            }
            BackendRequest::Storage(address, key, sender) => {
                if let Some(value) = self.db.storage(&address, &key) {
                    let _ = sender.send(Ok(value));
                    return;
                }
                let waiters = self.storage_requests.entry((address, key)).or_default();
                waiters.push(sender);
                if waiters.len() == 1 {
                    let provider = self.provider.clone();
                    let block_id = self.block_id;
                    self.pending.push(Box::pin(async move {
                        let value = provider.get_storage_at(address, key).block_id(block_id).await;
                        ProviderResponse::Storage(address, key, value.map_err(Arc::new))
                    }));
                }
            }
            BackendRequest::BlockHash(number, sender) => {
                if let Some(hash) = self.db.block_hash(&U256::from(number)) {
                    let _ = sender.send(Ok(hash));
                    return;
                }
                let waiters = self.block_requests.entry(number).or_default();
                waiters.push(sender);
                if waiters.len() == 1 {
                    let provider = self.provider.clone();
                    self.pending.push(Box::pin(async move {
                        let hash = provider
                            .get_block_by_number(BlockNumberOrTag::Number(number))
                            .await
                            .map(|block| block.map(|block| block.header().hash()));
                        ProviderResponse::BlockHash(number, hash.map_err(Arc::new))
                    }));
                }
            }
            BackendRequest::FullBlock(block_id, sender) => {
                let provider = self.provider.clone();
                self.pending.push(Box::pin(async move {
                    let block = match provider.get_block(block_id).full().await {
                        Ok(Some(block)) => Ok(block),
                        Ok(None) => Err(ForkError::BlockNotFound(block_id)),
                        Err(err) => Err(ForkError::GetFullBlock(block_id, Arc::new(err))),
                    };
                    let _ = sender.send(block);
                    ProviderResponse::Done
                }));
            }
            BackendRequest::Transaction(hash, sender) => {
                let provider = self.provider.clone();
                self.pending.push(Box::pin(async move {
                    let tx = match provider.get_transaction_by_hash(hash).await {
                        Ok(Some(tx)) => Ok(tx),
                        Ok(None) => Err(ForkError::TransactionNotFound(hash)),
                        Err(err) => Err(ForkError::GetTransaction(hash, Arc::new(err))),
                    };
                    let _ = sender.send(tx);
                    ProviderResponse::Done
                }));
            }
            BackendRequest::SetPinnedBlock(block_id) => self.block_id = block_id,
            BackendRequest::UpdateAccounts(accounts) => self.db.extend_accounts(accounts),
            BackendRequest::UpdateStorage(storage) => self.db.extend_storage(storage),
            BackendRequest::UpdateBlockHashes(hashes) => self.db.extend_block_hashes(hashes),
            BackendRequest::Any(future) => {
                self.pending.push(Box::pin(async move {
                    future.await;
                    ProviderResponse::Done
                }));
            }
        }
    }

    fn on_response(&mut self, response: ProviderResponse) {
        match response {
            ProviderResponse::Account(address, result) => {
                let result = match result {
                    Ok(info) => {
                        self.db.insert_account(address, info.clone());
                        Ok(info)
                    }
                    Err(err) => Err(ForkError::GetAccount(address, err)),
                };
                for sender in self.account_requests.remove(&address).unwrap_or_default() {
                    let _ = sender.send(result.clone());
                }
            }
            ProviderResponse::Storage(address, key, result) => {
                let result = match result {
                    Ok(value) => {
                        self.db.insert_storage(address, key, value);
                        Ok(value)
                    }
                    Err(err) => Err(ForkError::GetStorage(address, key, err)),
                };
                for sender in self.storage_requests.remove(&(address, key)).unwrap_or_default() {
                    let _ = sender.send(result.clone());
                }
            }
            ProviderResponse::BlockHash(number, result) => {
                let result = match result {
                    Ok(Some(hash)) => {
                        self.db.insert_block_hash(U256::from(number), hash);
                        Ok(hash)
                    }
                    // A missing block is never cached: it may still be mined.
                    Ok(None) => Err(ForkError::BlockNotFound(number.into())),
                    Err(err) => Err(ForkError::GetBlockHash(number, err)),
                };
                for sender in self.block_requests.remove(&number).unwrap_or_default() {
                    let _ = sender.send(result.clone());
                }
            }
            ProviderResponse::Done => {}
        }
    }
}

/// Fetches an account with the endpoint's supported method, recording it in `mode`.
///
/// An account with no balance, nonce or code is reported as absent: since EIP-161 an empty
/// account is indistinguishable from a missing one, and JSON-RPC cannot tell them apart.
async fn fetch_account<N: Network>(
    provider: DynProvider<N>,
    address: Address,
    block_id: BlockId,
    mode: Arc<AtomicU8>,
) -> Result<Option<AccountInfo>, TransportError> {
    let separate = || async {
        let balance = provider.get_balance(address).block_id(block_id);
        let nonce = provider.get_transaction_count(address).block_id(block_id);
        let code = provider.get_code_at(address).block_id(block_id);
        tokio::try_join!(balance, nonce, code)
    };
    let account_info = || async {
        let info = provider.get_account_info(address).block_id(block_id).await?;
        Ok::<_, TransportError>((info.balance, info.nonce, info.code))
    };
    let (balance, nonce, code) = match mode.load(Ordering::Relaxed) {
        FETCH_UNCHECKED => {
            let mut info = pin!(account_info());
            let mut triple = pin!(separate());
            // Biased so `eth_getAccountInfo` is sent first and always probed.
            tokio::select! {
                biased;
                info = &mut info => match info {
                    Ok(info) => {
                        mode.store(FETCH_ACCOUNT_INFO, Ordering::Relaxed);
                        info
                    }
                    Err(err) => {
                        tracing::trace!(target: "evm2::fork", ?err, "eth_getAccountInfo unsupported");
                        mode.store(FETCH_SEPARATE, Ordering::Relaxed);
                        triple.await?
                    }
                },
                triple = &mut triple => {
                    let triple = triple?;
                    mode.store(FETCH_SEPARATE, Ordering::Relaxed);
                    triple
                }
            }
        }
        FETCH_ACCOUNT_INFO => match account_info().await {
            Ok(info) => info,
            // A load-balanced endpoint may route to an instance without the method.
            Err(_) => {
                mode.store(FETCH_SEPARATE, Ordering::Relaxed);
                separate().await?
            }
        },
        _ => separate().await?,
    };
    Ok(account_from_parts(balance, nonce, code))
}

fn account_from_parts(balance: U256, nonce: u64, code: Bytes) -> Option<AccountInfo> {
    if balance.is_zero() && nonce == 0 && code.is_empty() {
        return None;
    }
    Some(
        AccountInfo::default()
            .with_balance(balance)
            .with_nonce(nonce)
            .with_code(Bytecode::from_stored(code)),
    )
}

/// A cloneable handle to a forked chain that implements [`DatabaseRef`].
///
/// Reads hit the shared [`BlockchainDb`] first and only reach the [`BackendHandler`] on a miss,
/// blocking the calling thread (see [`BlockingMode`]) until the handler answers. Clones share the
/// cache and the handler, so worker threads can fork the same chain without duplicate RPC calls.
///
/// Blocking reads must not run on the thread that drives the handler: use
/// [`Self::spawn_backend_thread`] unless the handler runs on a multi-threaded Tokio runtime.
pub struct SharedBackend<N: Network = Ethereum> {
    backend: UnboundedSender<BackendRequest<N>>,
    db: BlockchainDb,
    blocking_mode: BlockingMode,
}

impl<N: Network> Clone for SharedBackend<N> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            db: self.db.clone(),
            blocking_mode: self.blocking_mode,
        }
    }
}

impl<N: Network> fmt::Debug for SharedBackend<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedBackend")
            .field("db", &self.db)
            .field("blocking_mode", &self.blocking_mode)
            .finish_non_exhaustive()
    }
}

impl<N: Network> SharedBackend<N> {
    /// Creates a handle and the handler it talks to, without running the handler.
    ///
    /// `pin_block` is the block state is read at; the latest block when `None`. Pin a block
    /// number or hash for consistent reads: `latest` moves while the fork runs.
    pub fn new<P: Provider<N> + 'static>(
        provider: P,
        db: BlockchainDb,
        pin_block: Option<BlockId>,
    ) -> (Self, BackendHandler<N>) {
        let (backend, incoming) = unbounded_channel();
        let account_fetch_mode =
            Arc::new(AtomicU8::new(db.meta().account_fetch_policy.initial_mode()));
        let handler = BackendHandler {
            provider: provider.erased(),
            db: db.clone(),
            incoming,
            block_id: pin_block.unwrap_or_default(),
            account_fetch_mode,
            account_requests: Default::default(),
            storage_requests: Default::default(),
            block_requests: Default::default(),
            pending: FuturesUnordered::new(),
        };
        (Self { backend, db, blocking_mode: BlockingMode::default() }, handler)
    }

    /// Spawns the handler on the current Tokio runtime and returns a handle to it.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn spawn_backend<P: Provider<N> + 'static>(
        provider: P,
        db: BlockchainDb,
        pin_block: Option<BlockId>,
    ) -> Self {
        let (backend, handler) = Self::new(provider, db, pin_block);
        tokio::spawn(handler.run());
        backend
    }

    /// Spawns the handler on a dedicated thread with its own Tokio runtime and returns a handle
    /// to it.
    ///
    /// The thread exits once every handle is dropped.
    ///
    /// # Panics
    ///
    /// Panics if the thread or its runtime cannot be created.
    pub fn spawn_backend_thread<P: Provider<N> + 'static>(
        provider: P,
        db: BlockchainDb,
        pin_block: Option<BlockId>,
    ) -> Self {
        let (backend, handler) = Self::new(provider, db, pin_block);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create fork backend runtime");
        thread::Builder::new()
            .name("evm2-fork-backend".into())
            .spawn(move || runtime.block_on(handler.run()))
            .expect("failed to spawn fork backend thread");
        backend
    }

    /// Returns a handle to the same backend that blocks with `mode`.
    #[inline]
    pub fn with_blocking_mode(&self, mode: BlockingMode) -> Self {
        Self { blocking_mode: mode, ..self.clone() }
    }

    /// Returns the shared cache.
    #[inline]
    pub const fn data(&self) -> &BlockchainDb {
        &self.db
    }

    /// Pins later fetches to `block`.
    ///
    /// Entries already cached are kept; clear [`Self::data`] to drop them.
    pub fn set_pinned_block(&self, block: impl Into<BlockId>) -> ForkResult<()> {
        self.send(BackendRequest::SetPinnedBlock(block.into()))
    }

    /// Fetches a block with its full transactions. Blocks are not cached.
    pub fn get_full_block(&self, block: impl Into<BlockId>) -> ForkResult<N::BlockResponse> {
        let block = block.into();
        self.request(|sender| BackendRequest::FullBlock(block, sender))
    }

    /// Fetches a transaction by hash. Transactions are not cached.
    pub fn get_transaction(&self, hash: B256) -> ForkResult<N::TransactionResponse> {
        self.request(|sender| BackendRequest::Transaction(hash, sender))
    }

    /// Inserts or replaces accounts in the cache, through the handler.
    pub fn insert_or_update_address(&self, accounts: AddressMap<Option<AccountInfo>>) {
        self.send_update(BackendRequest::UpdateAccounts(accounts));
    }

    /// Inserts or replaces accounts' storage in the cache, through the handler.
    pub fn insert_or_update_storage(&self, storage: AddressMap<U256Map<Word>>) {
        self.send_update(BackendRequest::UpdateStorage(storage));
    }

    /// Inserts or replaces block hashes in the cache, through the handler.
    pub fn insert_or_update_block_hashes(&self, block_hashes: U256Map<B256>) {
        self.send_update(BackendRequest::UpdateBlockHashes(block_hashes));
    }

    /// Runs `future` on the handler's runtime and blocks until it finishes.
    ///
    /// Use it to make any other async request, such as a provider call, from synchronous code
    /// that cannot reach a runtime itself.
    pub fn do_any_request<T, E, F>(&self, future: F) -> ForkResult<T>
    where
        F: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Into<Box<dyn Error + Send + Sync>> + 'static,
    {
        self.request(|sender| {
            BackendRequest::Any(Box::pin(async move {
                let result =
                    future.await.map_err(|err| ForkError::AnyRequest(Arc::from(err.into())));
                let _ = sender.send(result);
            }))
        })
    }

    /// Writes the cache to its cache path, if any.
    pub fn flush_cache(&self) -> std::io::Result<()> {
        self.db.flush()
    }

    /// Writes the cache to `path`.
    pub fn flush_cache_to(&self, path: &Path) -> std::io::Result<()> {
        self.db.flush_to(path)
    }

    /// Returns a copy of every cached account.
    pub fn accounts(&self) -> AddressMap<Option<AccountInfo>> {
        self.db.accounts()
    }

    /// Returns the number of cached accounts.
    pub fn accounts_len(&self) -> usize {
        self.db.accounts_len()
    }

    /// Returns a copy of every cached storage slot.
    pub fn storage(&self) -> AddressMap<U256Map<Word>> {
        self.db.storages()
    }

    /// Returns the number of accounts with cached storage.
    pub fn storage_len(&self) -> usize {
        self.db.storage_len()
    }

    /// Returns a copy of every cached block hash.
    pub fn block_hashes(&self) -> U256Map<B256> {
        self.db.block_hashes()
    }

    /// Returns the number of cached block hashes.
    pub fn block_hashes_len(&self) -> usize {
        self.db.block_hashes_len()
    }

    /// Fetches an account into the cache ahead of execution.
    ///
    /// A prefetched entry is left out of the cache file until something reads it, so a wrong
    /// prediction costs one RPC request and is not carried over to the next run.
    pub fn prefetch_basic(&self, address: Address) -> ForkResult<()> {
        if self.db.account(&address).is_some() {
            return Ok(());
        }
        self.request(|sender| BackendRequest::Account(address, sender))?;
        self.db.mark_prefetched(PrefetchKey::Account(address));
        Ok(())
    }

    /// Fetches a storage slot into the cache ahead of execution, like [`Self::prefetch_basic`].
    pub fn prefetch_storage(&self, address: Address, key: Word) -> ForkResult<()> {
        if self.db.storage(&address, &key).is_some() {
            return Ok(());
        }
        self.request(|sender| BackendRequest::Storage(address, key, sender))?;
        self.db.mark_prefetched(PrefetchKey::Slot(address, key));
        Ok(())
    }

    fn send(&self, request: BackendRequest<N>) -> ForkResult<()> {
        self.backend.send(request).map_err(|_| ForkError::BackendShutdown)
    }

    fn send_update(&self, request: BackendRequest<N>) {
        if self.send(request).is_err() {
            tracing::error!(target: "evm2::fork", "failed to send cache update: backend shut down");
        }
    }

    fn request<T>(
        &self,
        request: impl FnOnce(ResultSender<T>) -> BackendRequest<N>,
    ) -> ForkResult<T> {
        self.blocking_mode.run(|| {
            let (sender, receiver) = mpsc::channel();
            self.send(request(sender))?;
            receiver.recv().map_err(|_| ForkError::BackendShutdown)?
        })
    }
}

fn log_error(err: &ForkError) {
    tracing::error!(target: "evm2::fork", %err, "fork backend request failed");
    if err.is_possibly_non_archive_node_error() {
        tracing::error!(target: "evm2::fork", "{NON_ARCHIVE_NODE_WARNING}");
    }
}

impl<N: Network> DatabaseRef for SharedBackend<N> {
    type Error = ForkError;

    fn get_account(&self, address: &Address) -> ForkResult<Option<AccountInfo>> {
        self.db.note_read(PrefetchKey::Account(*address));
        if let Some(info) = self.db.account(address) {
            return Ok(info);
        }
        self.request(|sender| BackendRequest::Account(*address, sender)).inspect_err(log_error)
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> ForkResult<Bytecode> {
        self.db.code_by_hash(code_hash).ok_or(ForkError::MissingCode(*code_hash))
    }

    fn get_storage(&self, address: &Address, key: &Word) -> ForkResult<Word> {
        self.db.note_read(PrefetchKey::Slot(*address, *key));
        if let Some(value) = self.db.storage(address, key) {
            return Ok(value);
        }
        self.request(|sender| BackendRequest::Storage(*address, *key, sender))
            .inspect_err(log_error)
    }

    fn get_block_hash(&self, number: &Word) -> ForkResult<B256> {
        if let Some(hash) = self.db.block_hash(number) {
            return Ok(hash);
        }
        let number = u64::try_from(*number).map_err(|_| ForkError::InvalidBlockNumber(*number))?;
        self.request(|sender| BackendRequest::BlockHash(number, sender)).inspect_err(log_error)
    }
}
