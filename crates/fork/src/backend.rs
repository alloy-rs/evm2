//! Background RPC handler and the shared database handle that talks to it.

use crate::{BlockchainDb, ForkError, ForkResult};
use alloy_eips::BlockId;
use alloy_primitives::{
    Address, B256, U256,
    map::{AddressMap, HashMap},
};
use alloy_provider::{
    Network, Provider,
    network::{BlockResponse, primitives::HeaderResponse},
};
use alloy_rpc_types_eth::BlockNumberOrTag;
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, DatabaseRef},
    interpreter::Word,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    fmt,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::mpsc::{self, Sender},
    thread,
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

type AccountSender = Sender<ForkResult<Option<AccountInfo>>>;
type StorageSender = Sender<ForkResult<Word>>;
type BlockHashSender = Sender<ForkResult<B256>>;
type ProviderFuture = Pin<Box<dyn Future<Output = ProviderResponse> + Send>>;

/// A request from a [`SharedBackend`] to its [`BackendHandler`].
enum BackendRequest {
    Account(Address, AccountSender),
    Storage(Address, Word, StorageSender),
    BlockHash(u64, BlockHashSender),
    SetPinnedBlock(BlockId),
}

/// A finished RPC fetch.
enum ProviderResponse {
    Account(Address, ForkResult<Option<AccountInfo>>),
    Storage(Address, Word, ForkResult<Word>),
    BlockHash(u64, ForkResult<B256>),
}

/// Serves [`SharedBackend`] requests by fetching remote state through a provider.
///
/// Every request is first checked against the shared [`BlockchainDb`]. Missing entries are
/// fetched at the pinned block, and concurrent requests for the same account, slot or block hash
/// wait on one in-flight fetch. Results are stored in the cache before waiters are answered.
///
/// The handler runs until every [`SharedBackend`] handle is dropped; drive it with
/// [`Self::run`] or let [`SharedBackend::spawn_backend`] or
/// [`SharedBackend::spawn_backend_thread`] do so.
#[must_use = "the handler does nothing unless run"]
pub struct BackendHandler<P, N> {
    provider: P,
    db: BlockchainDb,
    incoming: UnboundedReceiver<BackendRequest>,
    block_id: BlockId,
    account_requests: AddressMap<Vec<AccountSender>>,
    storage_requests: HashMap<(Address, Word), Vec<StorageSender>>,
    block_requests: HashMap<u64, Vec<BlockHashSender>>,
    pending: FuturesUnordered<ProviderFuture>,
    _network: PhantomData<fn() -> N>,
}

impl<P, N> fmt::Debug for BackendHandler<P, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackendHandler")
            .field("block_id", &self.block_id)
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl<P, N> BackendHandler<P, N>
where
    P: Provider<N> + Clone + 'static,
    N: Network,
{
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

    fn on_request(&mut self, request: BackendRequest) {
        match request {
            BackendRequest::Account(address, sender) => {
                if let Some(info) = self.db.account(&address) {
                    let _ = sender.send(Ok(info));
                    return;
                }
                let waiters = self.account_requests.entry(address).or_default();
                waiters.push(sender);
                if waiters.len() == 1 {
                    let fetch = fetch_account(self.provider.clone(), address, self.block_id);
                    self.pending.push(Box::pin(async move {
                        ProviderResponse::Account(address, fetch.await)
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
                        let value = provider
                            .get_storage_at(address, key)
                            .block_id(block_id)
                            .await
                            .map_err(ForkError::from);
                        ProviderResponse::Storage(address, key, value)
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
                            .map_err(ForkError::from)
                            .and_then(|block| {
                                block
                                    .map(|block| block.header().hash())
                                    .ok_or(ForkError::BlockNotFound(number))
                            });
                        ProviderResponse::BlockHash(number, hash)
                    }));
                }
            }
            BackendRequest::SetPinnedBlock(block_id) => self.block_id = block_id,
        }
    }

    fn on_response(&mut self, response: ProviderResponse) {
        match response {
            ProviderResponse::Account(address, result) => {
                if let Ok(info) = &result {
                    self.db.insert_account(address, info.clone());
                }
                for sender in self.account_requests.remove(&address).unwrap_or_default() {
                    let _ = sender.send(result.clone());
                }
            }
            ProviderResponse::Storage(address, key, result) => {
                if let Ok(value) = &result {
                    self.db.insert_storage(address, key, *value);
                }
                for sender in self.storage_requests.remove(&(address, key)).unwrap_or_default() {
                    let _ = sender.send(result.clone());
                }
            }
            ProviderResponse::BlockHash(number, result) => {
                if let Ok(hash) = &result {
                    self.db.insert_block_hash(U256::from(number), *hash);
                }
                for sender in self.block_requests.remove(&number).unwrap_or_default() {
                    let _ = sender.send(result.clone());
                }
            }
        }
    }
}

/// Fetches balance, nonce and code concurrently.
///
/// An account with no balance, nonce or code is reported as absent: since EIP-161 an empty
/// account is indistinguishable from a missing one, and JSON-RPC cannot tell them apart.
async fn fetch_account<P, N>(
    provider: P,
    address: Address,
    block_id: BlockId,
) -> ForkResult<Option<AccountInfo>>
where
    P: Provider<N>,
    N: Network,
{
    let balance = provider.get_balance(address).block_id(block_id);
    let nonce = provider.get_transaction_count(address).block_id(block_id);
    let code = provider.get_code_at(address).block_id(block_id);
    let (balance, nonce, code) = tokio::try_join!(balance, nonce, code)?;
    if balance.is_zero() && nonce == 0 && code.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        AccountInfo::default()
            .with_balance(balance)
            .with_nonce(nonce)
            .with_code(Bytecode::from_stored(code)),
    ))
}

/// A cloneable handle to a forked chain that implements [`DatabaseRef`].
///
/// Reads hit the shared [`BlockchainDb`] first and only reach the [`BackendHandler`] on a miss,
/// blocking the calling thread until the handler answers. Clones share the cache and the handler,
/// so worker threads can fork the same chain without duplicate RPC calls.
///
/// Blocking reads must not run on the thread that drives the handler: use
/// [`Self::spawn_backend_thread`] unless the handler runs on a multi-threaded Tokio runtime,
/// where reads made from that runtime's workers are moved off it with
/// [`tokio::task::block_in_place`].
#[derive(Clone, Debug)]
pub struct SharedBackend {
    backend: UnboundedSender<BackendRequest>,
    db: BlockchainDb,
}

impl SharedBackend {
    /// Creates a handle and the handler it talks to, without running the handler.
    ///
    /// `pin_block` is the block state is read at; the latest block when `None`. Pin a block
    /// number for consistent reads: `latest` moves while the fork runs.
    pub fn new<P, N>(
        provider: P,
        db: BlockchainDb,
        pin_block: Option<BlockId>,
    ) -> (Self, BackendHandler<P, N>)
    where
        P: Provider<N> + Clone + 'static,
        N: Network,
    {
        let (backend, incoming) = unbounded_channel();
        let handler = BackendHandler {
            provider,
            db: db.clone(),
            incoming,
            block_id: pin_block.unwrap_or_default(),
            account_requests: Default::default(),
            storage_requests: Default::default(),
            block_requests: Default::default(),
            pending: FuturesUnordered::new(),
            _network: PhantomData,
        };
        (Self { backend, db }, handler)
    }

    /// Spawns the handler on the current Tokio runtime and returns a handle to it.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn spawn_backend<P, N>(provider: P, db: BlockchainDb, pin_block: Option<BlockId>) -> Self
    where
        P: Provider<N> + Clone + 'static,
        N: Network,
    {
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
    pub fn spawn_backend_thread<P, N>(
        provider: P,
        db: BlockchainDb,
        pin_block: Option<BlockId>,
    ) -> Self
    where
        P: Provider<N> + Clone + 'static,
        N: Network,
    {
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

    /// Returns the shared cache.
    #[inline]
    pub const fn data(&self) -> &BlockchainDb {
        &self.db
    }

    /// Pins later fetches to `block`.
    ///
    /// Entries already cached are kept; clear [`Self::data`] to drop them.
    pub fn set_pinned_block(&self, block: impl Into<BlockId>) -> ForkResult<()> {
        self.backend
            .send(BackendRequest::SetPinnedBlock(block.into()))
            .map_err(|_| ForkError::BackendShutdown)
    }

    /// Writes the cache to its cache path, if any.
    pub fn flush_cache(&self) -> std::io::Result<()> {
        self.db.flush()
    }

    fn request<T>(
        &self,
        request: impl FnOnce(Sender<ForkResult<T>>) -> BackendRequest,
    ) -> ForkResult<T> {
        let (sender, receiver) = mpsc::channel();
        self.backend.send(request(sender)).map_err(|_| ForkError::BackendShutdown)?;
        let recv = || receiver.recv().map_err(|_| ForkError::BackendShutdown)?;
        match Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(recv)
            }
            _ => recv(),
        }
    }
}

impl DatabaseRef for SharedBackend {
    type Error = ForkError;

    fn get_account(&self, address: &Address) -> ForkResult<Option<AccountInfo>> {
        if let Some(info) = self.db.account(address) {
            return Ok(info);
        }
        self.request(|sender| BackendRequest::Account(*address, sender))
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> ForkResult<Bytecode> {
        self.db.code_by_hash(code_hash).ok_or(ForkError::MissingCode(*code_hash))
    }

    fn get_storage(&self, address: &Address, key: &Word) -> ForkResult<Word> {
        if let Some(value) = self.db.storage(address, key) {
            return Ok(value);
        }
        self.request(|sender| BackendRequest::Storage(*address, *key, sender))
    }

    fn get_block_hash(&self, number: &Word) -> ForkResult<B256> {
        if let Some(hash) = self.db.block_hash(number) {
            return Ok(hash);
        }
        let number_u64 =
            u64::try_from(*number).map_err(|_| ForkError::InvalidBlockNumber(*number))?;
        self.request(|sender| BackendRequest::BlockHash(number_u64, sender))
    }
}
