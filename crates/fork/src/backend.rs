//! Background RPC handler and the shared database handle that talks to it.

use crate::{
    BlockchainDb, ForkError, ForkResult, NON_ARCHIVE_NODE_WARNING,
    cache::PrefetchKey,
    prestate::{self, Prediction},
};
use alloy_eips::BlockId;
use alloy_primitives::{
    Address, B256, Bytes, U64, U256,
    map::{AddressMap, Entry, HashMap, U256Map},
};
use alloy_provider::{
    DynProvider, Network, Provider,
    network::{BlockResponse, Ethereum, primitives::HeaderResponse},
    transport::{
        TransportError,
        layers::{RateLimitRetryPolicy, RetryPolicy},
    },
};
use alloy_rpc_types_eth::{AccountInfo as RpcAccountInfo, BlockNumberOrTag};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, DatabaseRef},
    interpreter::Word,
};
use futures_util::{
    StreamExt,
    stream::{self, FuturesUnordered},
};
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

/// Keys per JSON-RPC batch when prefetching.
const PREFETCH_BATCH_SIZE: usize = 50;

/// JSON-RPC batches in flight at once when prefetching.
const PREFETCH_CONCURRENCY: usize = 4;

/// Times a prefetched key is fetched again after a rate limit or another retryable error.
const PREFETCH_RETRIES: u32 = 4;

/// Wait before the first retry of a prefetch, doubled for each later one.
const PREFETCH_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

/// What a prefetch fetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PrefetchStats {
    /// Accounts inserted into the cache.
    pub accounts: usize,
    /// Storage slots inserted into the cache.
    pub slots: usize,
    /// Keys whose fetch failed; a later read fetches them again.
    pub failed: usize,
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
    /// Pins to a block and clears the cache, in order with other requests.
    Repin(BlockId, ResultSender<()>),
    UpdateAccounts(AddressMap<Option<AccountInfo>>),
    UpdateStorage(AddressMap<U256Map<Word>>),
    UpdateBlockHashes(U256Map<B256>),
    /// A future that answers its own sender.
    Any(BoxFuture<()>),
    Prefetch(Vec<Address>, Vec<(Address, Word)>, ResultSender<PrefetchStats>),
    PrefetchBlock(BlockId, ResultSender<PrefetchStats>),
}

/// A key fetched in a prefetch batch.
#[derive(Clone, Copy, Debug)]
enum FetchKey {
    Account(Address),
    Slot(Address, Word),
}

/// A finished RPC fetch whose result is cached and fanned out to waiters.
enum ProviderResponse {
    Account(Address, RpcResult<Option<AccountInfo>>),
    Storage(Address, Word, RpcResult<Word>),
    BlockHash(u64, RpcResult<Option<B256>>),
    /// A request that answered its sender itself.
    Done,
    /// The results of a prefetch's batches.
    Batch(Vec<Self>, ResultSender<PrefetchStats>),
    /// Exact state predicted for a block.
    Prestate(
        Vec<(Address, Option<AccountInfo>)>,
        Vec<(Address, Word, Word)>,
        ResultSender<PrefetchStats>,
    ),
    /// Keys predicted for a block, still to be fetched.
    PrefetchKeys(Vec<Address>, Vec<(Address, Word)>, ResultSender<PrefetchStats>),
    /// A prediction that failed.
    PrefetchFailed(ForkError, ResultSender<PrefetchStats>),
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
    /// Bumped on every pin change; fetches started under an earlier one are not cached.
    generation: u64,
    pending: FuturesUnordered<BoxFuture<(u64, ProviderResponse)>>,
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
                Some((generation, response)) = self.pending.next(), if !self.pending.is_empty() => {
                    if generation == self.generation {
                        self.on_response(response);
                    } else {
                        self.on_stale(response);
                    }
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
                    self.push_account_fetch(address);
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
                    self.push_storage_fetch(address, key);
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
                    self.push_pending(Box::pin(async move {
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
                self.push_pending(Box::pin(async move {
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
                self.push_pending(Box::pin(async move {
                    let tx = match provider.get_transaction_by_hash(hash).await {
                        Ok(Some(tx)) => Ok(tx),
                        Ok(None) => Err(ForkError::TransactionNotFound(hash)),
                        Err(err) => Err(ForkError::GetTransaction(hash, Arc::new(err))),
                    };
                    let _ = sender.send(tx);
                    ProviderResponse::Done
                }));
            }
            BackendRequest::SetPinnedBlock(block_id) => {
                self.block_id = block_id;
                self.generation += 1;
            }
            BackendRequest::Repin(block_id, sender) => {
                self.block_id = block_id;
                self.generation += 1;
                self.db.clear();
                let _ = sender.send(Ok(()));
            }
            BackendRequest::UpdateAccounts(accounts) => self.db.extend_accounts(accounts),
            BackendRequest::UpdateStorage(storage) => self.db.extend_storage(storage),
            BackendRequest::UpdateBlockHashes(hashes) => self.db.extend_block_hashes(hashes),
            BackendRequest::Any(future) => {
                self.push_pending(Box::pin(async move {
                    future.await;
                    ProviderResponse::Done
                }));
            }
            BackendRequest::Prefetch(accounts, slots, sender) => {
                self.start_prefetch(accounts, slots, sender);
            }
            BackendRequest::PrefetchBlock(block, sender) => {
                let provider = self.provider.clone();
                let state_block = self.block_id;
                self.push_pending(Box::pin(async move {
                    match prestate::predict(&provider, block, state_block).await {
                        Ok(Prediction::State { accounts, slots }) => {
                            ProviderResponse::Prestate(accounts, slots, sender)
                        }
                        Ok(Prediction::Keys { accounts, slots }) => {
                            ProviderResponse::PrefetchKeys(accounts, slots, sender)
                        }
                        Err(err) => ProviderResponse::PrefetchFailed(err, sender),
                    }
                }));
            }
        }
    }

    /// Queues a fetch under the current pin.
    fn push_pending(&mut self, future: BoxFuture<ProviderResponse>) {
        let generation = self.generation;
        self.pending.push(Box::pin(async move { (generation, future.await) }));
    }

    /// Handles a response fetched under an earlier pin: its state is not cached, and readers
    /// waiting on it are served by a fetch at the current pin.
    fn on_stale(&mut self, response: ProviderResponse) {
        match response {
            ProviderResponse::Account(address, _) => {
                if self.account_requests.get(&address).is_some_and(|w| !w.is_empty()) {
                    self.push_account_fetch(address);
                } else {
                    self.account_requests.remove(&address);
                }
            }
            ProviderResponse::Storage(address, key, _) => {
                if self.storage_requests.get(&(address, key)).is_some_and(|w| !w.is_empty()) {
                    self.push_storage_fetch(address, key);
                } else {
                    self.storage_requests.remove(&(address, key));
                }
            }
            // Block hashes do not depend on the pin, and other responses answered themselves.
            ProviderResponse::BlockHash(..) | ProviderResponse::Done => self.on_response(response),
            ProviderResponse::Batch(responses, sender) => {
                for response in responses {
                    self.on_stale(response);
                }
                let _ = sender.send(Ok(PrefetchStats::default()));
            }
            ProviderResponse::Prestate(.., sender) | ProviderResponse::PrefetchKeys(.., sender) => {
                let _ = sender.send(Ok(PrefetchStats::default()));
            }
            ProviderResponse::PrefetchFailed(err, sender) => {
                let _ = sender.send(Err(err));
            }
        }
    }

    fn push_account_fetch(&mut self, address: Address) {
        let fetch = fetch_account(
            self.provider.clone(),
            address,
            self.block_id,
            Arc::clone(&self.account_fetch_mode),
        );
        self.push_pending(Box::pin(async move {
            ProviderResponse::Account(address, fetch.await.map_err(Arc::new))
        }));
    }

    fn push_storage_fetch(&mut self, address: Address, key: Word) {
        let provider = self.provider.clone();
        let block_id = self.block_id;
        self.push_pending(Box::pin(async move {
            let value = provider.get_storage_at(address, key).block_id(block_id).await;
            ProviderResponse::Storage(address, key, value.map_err(Arc::new))
        }));
    }

    /// Fetches every key not cached or in flight in concurrent JSON-RPC batches.
    ///
    /// Each key is registered as in flight without waiters, so a read that arrives meanwhile
    /// waits on the batch instead of fetching it again.
    fn start_prefetch(
        &mut self,
        accounts: Vec<Address>,
        slots: Vec<(Address, Word)>,
        sender: ResultSender<PrefetchStats>,
    ) {
        let mut keys = Vec::new();
        for address in accounts {
            if self.db.account(&address).is_none()
                && let Entry::Vacant(entry) = self.account_requests.entry(address)
            {
                entry.insert(Vec::new());
                keys.push(FetchKey::Account(address));
            }
        }
        for (address, key) in slots {
            if self.db.storage(&address, &key).is_none()
                && let Entry::Vacant(entry) = self.storage_requests.entry((address, key))
            {
                entry.insert(Vec::new());
                keys.push(FetchKey::Slot(address, key));
            }
        }
        if keys.is_empty() {
            let _ = sender.send(Ok(PrefetchStats::default()));
            return;
        }
        let provider = self.provider.clone();
        let block_id = self.block_id;
        let mode = Arc::clone(&self.account_fetch_mode);
        self.push_pending(Box::pin(async move {
            let mut responses = Vec::with_capacity(keys.len());
            let mut keys = keys;
            for attempt in 0..=PREFETCH_RETRIES {
                let chunks =
                    keys.chunks(PREFETCH_BATCH_SIZE).map(<[FetchKey]>::to_vec).collect::<Vec<_>>();
                let batch = stream::iter(chunks)
                    .map(|chunk| fetch_batch(provider.clone(), chunk, block_id, Arc::clone(&mode)))
                    .buffer_unordered(PREFETCH_CONCURRENCY)
                    .flat_map(stream::iter)
                    .collect::<Vec<_>>()
                    .await;
                keys = Vec::new();
                for response in batch {
                    match &response {
                        ProviderResponse::Account(address, Err(err))
                            if attempt < PREFETCH_RETRIES
                                && RateLimitRetryPolicy::default().should_retry(err) =>
                        {
                            keys.push(FetchKey::Account(*address));
                        }
                        ProviderResponse::Storage(address, slot, Err(err))
                            if attempt < PREFETCH_RETRIES
                                && RateLimitRetryPolicy::default().should_retry(err) =>
                        {
                            keys.push(FetchKey::Slot(*address, *slot));
                        }
                        _ => responses.push(response),
                    }
                }
                if keys.is_empty() {
                    break;
                }
                tokio::time::sleep(PREFETCH_BACKOFF * 2u32.pow(attempt)).await;
            }
            ProviderResponse::Batch(responses, sender)
        }));
    }

    /// Caches the results of a prefetch. A failed key is fetched again on its own when a read
    /// waits on it, and dropped otherwise.
    fn on_batch(&mut self, responses: Vec<ProviderResponse>, sender: ResultSender<PrefetchStats>) {
        let mut stats = PrefetchStats::default();
        for response in responses {
            match &response {
                ProviderResponse::Account(address, result) => {
                    let waiting = self.account_requests.get(address).is_some_and(|w| !w.is_empty());
                    if result.is_err() {
                        stats.failed += 1;
                        if waiting {
                            self.push_account_fetch(*address);
                        } else {
                            self.account_requests.remove(address);
                        }
                        continue;
                    }
                    stats.accounts += 1;
                    if !waiting {
                        self.db.mark_prefetched(PrefetchKey::Account(*address));
                    }
                }
                ProviderResponse::Storage(address, key, result) => {
                    let waiting =
                        self.storage_requests.get(&(*address, *key)).is_some_and(|w| !w.is_empty());
                    if result.is_err() {
                        stats.failed += 1;
                        if waiting {
                            self.push_storage_fetch(*address, *key);
                        } else {
                            self.storage_requests.remove(&(*address, *key));
                        }
                        continue;
                    }
                    stats.slots += 1;
                    if !waiting {
                        self.db.mark_prefetched(PrefetchKey::Slot(*address, *key));
                    }
                }
                _ => {}
            }
            self.on_response(response);
        }
        let _ = sender.send(Ok(stats));
    }

    /// Caches exact predicted state, keeping entries already cached and answering reads waiting
    /// on the same keys.
    fn on_prestate(
        &mut self,
        accounts: Vec<(Address, Option<AccountInfo>)>,
        slots: Vec<(Address, Word, Word)>,
        sender: ResultSender<PrefetchStats>,
    ) {
        let mut stats = PrefetchStats::default();
        for (address, info) in accounts {
            if self.db.account(&address).is_some() {
                continue;
            }
            if !self.account_requests.get(&address).is_some_and(|w| !w.is_empty()) {
                self.db.mark_prefetched(PrefetchKey::Account(address));
            }
            stats.accounts += 1;
            self.on_response(ProviderResponse::Account(address, Ok(info)));
        }
        for (address, key, value) in slots {
            if self.db.storage(&address, &key).is_some() {
                continue;
            }
            if !self.storage_requests.get(&(address, key)).is_some_and(|w| !w.is_empty()) {
                self.db.mark_prefetched(PrefetchKey::Slot(address, key));
            }
            stats.slots += 1;
            self.on_response(ProviderResponse::Storage(address, key, Ok(value)));
        }
        let _ = sender.send(Ok(stats));
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
            ProviderResponse::Batch(responses, sender) => self.on_batch(responses, sender),
            ProviderResponse::Prestate(accounts, slots, sender) => {
                self.on_prestate(accounts, slots, sender);
            }
            ProviderResponse::PrefetchKeys(accounts, slots, sender) => {
                self.start_prefetch(accounts, slots, sender);
            }
            ProviderResponse::PrefetchFailed(err, sender) => {
                let _ = sender.send(Err(err));
            }
        }
    }
}

/// Fetches a batch of keys in one JSON-RPC batch request.
///
/// Accounts use `eth_getAccountInfo` unless the endpoint is known not to support it; if the
/// endpoint rejects it, the batch's accounts are fetched again with separate requests.
async fn fetch_batch<N: Network>(
    provider: DynProvider<N>,
    keys: Vec<FetchKey>,
    block_id: BlockId,
    mode: Arc<AtomicU8>,
) -> Vec<ProviderResponse> {
    enum Call<A, B, C, D> {
        Info(Address, A),
        Separate(Address, B, C, D),
        Slot(Address, Word, B),
    }

    let account_info = mode.load(Ordering::Relaxed) != FETCH_SEPARATE;
    let client = provider.client();
    let mut batch = alloy_rpc_client::BatchRequest::new(client);
    let mut calls = Vec::with_capacity(keys.len());
    let mut responses = Vec::with_capacity(keys.len());
    for key in keys {
        let call = match key {
            FetchKey::Account(address) if account_info => batch
                .add_call::<_, RpcAccountInfo>("eth_getAccountInfo", &(address, block_id))
                .map(|info| Call::Info(address, info)),
            FetchKey::Account(address) => (|| {
                let balance = batch.add_call::<_, U256>("eth_getBalance", &(address, block_id))?;
                let nonce =
                    batch.add_call::<_, U64>("eth_getTransactionCount", &(address, block_id))?;
                let code = batch.add_call::<_, Bytes>("eth_getCode", &(address, block_id))?;
                Ok(Call::Separate(address, balance, nonce, code))
            })(),
            FetchKey::Slot(address, slot) => batch
                .add_call::<_, U256>("eth_getStorageAt", &(address, slot, block_id))
                .map(|value| Call::Slot(address, slot, value)),
        };
        match call {
            Ok(call) => calls.push(call),
            Err(err) => responses.push(match key {
                FetchKey::Account(address) => {
                    ProviderResponse::Account(address, Err(Arc::new(err)))
                }
                FetchKey::Slot(address, slot) => {
                    ProviderResponse::Storage(address, slot, Err(Arc::new(err)))
                }
            }),
        }
    }
    if let Err(err) = batch.send().await {
        tracing::debug!(target: "evm2::fork", %err, "prefetch batch failed");
    }
    let mut retry = Vec::new();
    for call in calls {
        match call {
            Call::Info(address, info) => match info.await {
                Ok(info) => {
                    let _ = mode.compare_exchange(
                        FETCH_UNCHECKED,
                        FETCH_ACCOUNT_INFO,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                    responses.push(ProviderResponse::Account(
                        address,
                        Ok(account_from_parts(info.balance, info.nonce, info.code)),
                    ));
                }
                Err(err) if err.as_error_resp().is_some_and(|resp| resp.code == -32601) => {
                    mode.store(FETCH_SEPARATE, Ordering::Relaxed);
                    retry.push(FetchKey::Account(address));
                }
                Err(err) => responses.push(ProviderResponse::Account(address, Err(Arc::new(err)))),
            },
            Call::Separate(address, balance, nonce, code) => {
                let account = async { Ok((balance.await?, nonce.await?, code.await?)) }.await;
                responses.push(ProviderResponse::Account(
                    address,
                    account
                        .map(|(balance, nonce, code): (U256, U64, Bytes)| {
                            account_from_parts(balance, nonce.to(), code)
                        })
                        .map_err(Arc::new),
                ));
            }
            Call::Slot(address, slot, value) => {
                responses.push(ProviderResponse::Storage(
                    address,
                    slot,
                    value.await.map_err(Arc::new),
                ));
            }
        }
    }
    if !retry.is_empty() {
        responses.extend(Box::pin(fetch_batch(provider, retry, block_id, mode)).await);
    }
    responses
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

pub(crate) fn account_from_parts(balance: U256, nonce: u64, code: Bytes) -> Option<AccountInfo> {
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
            generation: 0,
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
    /// Entries already cached are kept; clear [`Self::data`] to drop them. Fetches still in flight
    /// at the old pin are not cached, and their readers get the state at `block`.
    pub fn set_pinned_block(&self, block: impl Into<BlockId>) -> ForkResult<()> {
        self.send(BackendRequest::SetPinnedBlock(block.into()))
    }

    /// Pins later fetches to `block` and clears the cache, after every request sent before it,
    /// and waits until it is done.
    pub(crate) fn repin(&self, block: BlockId) -> ForkResult<()> {
        self.request(|sender| BackendRequest::Repin(block, sender))
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

    /// Fetches accounts and storage slots into the cache in concurrent JSON-RPC batches, skipping
    /// keys already cached or in flight.
    ///
    /// Like [`Self::prefetch_basic`], entries nothing reads are left out of the cache file.
    pub fn prefetch(
        &self,
        accounts: impl IntoIterator<Item = Address>,
        slots: impl IntoIterator<Item = (Address, Word)>,
    ) -> ForkResult<PrefetchStats> {
        let accounts = accounts.into_iter().collect();
        let slots = slots.into_iter().collect();
        self.request(|sender| BackendRequest::Prefetch(accounts, slots, sender))
    }

    /// Predicts the state `block` reads from the pinned state and fetches it before execution.
    ///
    /// Pin the backend to `block`'s parent. The prestate comes from `debug_traceBlockByNumber`
    /// with the `prestateTracer` when the endpoint serves it; otherwise each transaction's
    /// `eth_createAccessList` names the keys, which are then fetched with [`Self::prefetch`].
    /// Replaying a block then makes a few batched requests instead of one blocking round trip
    /// per cold read. Reads the prediction missed are fetched as usual.
    pub fn prefetch_block(&self, block: impl Into<BlockId>) -> ForkResult<PrefetchStats> {
        let block = block.into();
        self.request(|sender| BackendRequest::PrefetchBlock(block, sender))
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
