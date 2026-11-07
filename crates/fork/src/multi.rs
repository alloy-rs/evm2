//! Several forks served from one background thread, modeled on Foundry's `MultiFork`.
//!
//! A [`MultiFork`] handle talks to one thread that runs a Tokio runtime owning every fork's
//! [`BackendHandler`](crate::BackendHandler). Forks of the same endpoint and block share one
//! backend, and therefore one cache and one handler.

use crate::{
    AccountFetchPolicy, BlockchainDb, BlockchainDbMeta, BlockingMode, ForkEnv, ForkError,
    ForkResult, SharedBackend, bal,
    env::fork_env,
    prefetch::{self, Prefetch},
};
use alloy_eips::{BlockId, eip7928::BlockAccessList};
use alloy_primitives::map::HashMap;
use alloy_provider::{
    DynProvider, Network, Provider, RootProvider,
    network::Ethereum,
    transport::{TransportErrorKind, layers::RetryBackoffLayer},
};
use alloy_rpc_client::{BuiltInConnectionString, RpcClient};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    env, fmt,
    future::{Future, pending},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinSet,
    time::{Instant, Interval},
};

/// How often every fork's cache is flushed unless [`MultiFork::set_flush_cache_interval`] says
/// otherwise, like Foundry.
const DEFAULT_FLUSH_CACHE_INTERVAL: Duration = Duration::from_secs(60);

/// Rate limit retries of the default HTTP connector, like Foundry's defaults.
const MAX_RATE_LIMIT_RETRIES: u32 = 10;

/// Initial backoff of the default HTTP connector, in milliseconds.
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Compute units per second the default HTTP connector budgets for.
const COMPUTE_UNITS_PER_SECOND: u64 = 330;

type Connector<N> = dyn Fn(&str) -> ForkResult<DynProvider<N>> + Send + Sync;
type CreateSender<N> = Sender<ForkResult<CreatedFork<N>>>;
type CreateFuture<N> = Pin<Box<dyn Future<Output = (u64, ForkResult<Resolved<N>>)> + Send>>;

/// The unique identifier of a fork: its endpoint and block, such as `https://rpc@0x10`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ForkId(pub String);

impl ForkId {
    /// Returns the identifier of a fork of `url` at block `number`, or at `latest` when `None`.
    pub fn new(url: &str, number: Option<u64>) -> Self {
        match number {
            Some(number) => Self(format!("{url}@{number:#x}")),
            None => Self(format!("{url}@latest")),
        }
    }

    /// Returns the identifier of a fork resolved to block `number`.
    fn resolved(url: &str, number: u64, state_by_number: bool) -> Self {
        let mut id = Self::new(url, Some(number));
        if state_by_number {
            id.0.push_str("#state-by-number");
        }
        id
    }

    /// Returns the identifier as a string.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ForkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Into<String>> From<T> for ForkId {
    fn from(id: T) -> Self {
        Self(id.into())
    }
}

/// Returns Foundry's RPC cache directory, `~/.foundry/cache/rpc`, when the home directory is
/// known.
///
/// Fork caches live at `<root>/<chain_id>/<block_number>` below it, so forks share their caches
/// with Foundry.
pub fn foundry_cache_root() -> Option<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join(".foundry").join("cache").join("rpc"))
}

/// Options of a fork of a remote chain, whose state is only available from `url`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateFork {
    /// The endpoint to read remote state from.
    pub url: String,
    /// The block to fork at; the latest block when `None`.
    pub block: Option<u64>,
    /// Whether to cache remote state on disk, below [`Self::cache_root`].
    pub enable_caching: bool,
    /// The directory holding disk caches, at `<root>/<chain_id>/<block_number>`.
    pub cache_root: Option<PathBuf>,
    /// Whether to read state at the block number instead of the exact block hash.
    ///
    /// A number can follow a replacement block after a reorganization, so such a fork never uses
    /// the disk cache.
    pub state_by_number: bool,
    /// Whether a fork of the latest block prefetches the keys cached for the newest older block.
    pub prefetch: bool,
    /// Whether to fill the cache from the fork block's access list (EIP-7928) before returning.
    pub prewarm_bal: bool,
    /// How the backend fetches accounts.
    pub account_fetch_policy: AccountFetchPolicy,
}

impl CreateFork {
    /// Whether a fork created with `self` may share the backend of one created with `other`
    /// at the same block: they must cache and fetch remote state the same way.
    fn shares_backend_with(&self, other: &Self) -> bool {
        self.enable_caching == other.enable_caching
            && self.cache_root == other.cache_root
            && self.prefetch == other.prefetch
            && self.account_fetch_policy == other.account_fetch_policy
    }

    /// Creates options for a fork of `url` at the latest block, without disk caching, with
    /// [`foundry_cache_root`] as the cache root.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            block: None,
            enable_caching: false,
            cache_root: foundry_cache_root(),
            state_by_number: false,
            prefetch: false,
            prewarm_bal: false,
            account_fetch_policy: Default::default(),
        }
    }

    /// Forks at `block`, or at the latest block when `None`.
    #[inline]
    pub const fn with_block(mut self, block: Option<u64>) -> Self {
        self.block = block;
        self
    }

    /// Enables or disables disk caching.
    #[inline]
    pub const fn with_caching(mut self, enable_caching: bool) -> Self {
        self.enable_caching = enable_caching;
        self
    }

    /// Sets the directory holding disk caches.
    #[inline]
    pub fn with_cache_root(mut self, cache_root: Option<PathBuf>) -> Self {
        self.cache_root = cache_root;
        self
    }

    /// Reads state at the block number instead of the block hash.
    #[inline]
    pub const fn with_state_by_number(mut self, state_by_number: bool) -> Self {
        self.state_by_number = state_by_number;
        self
    }

    /// Enables or disables prefetching for forks of the latest block.
    #[inline]
    pub const fn with_prefetch(mut self, prefetch: bool) -> Self {
        self.prefetch = prefetch;
        self
    }

    /// Enables or disables filling the cache from the fork block's access list.
    #[inline]
    pub const fn with_prewarm_bal(mut self, prewarm_bal: bool) -> Self {
        self.prewarm_bal = prewarm_bal;
        self
    }

    /// Sets how the backend fetches accounts.
    #[inline]
    pub const fn with_account_fetch_policy(mut self, policy: AccountFetchPolicy) -> Self {
        self.account_fetch_policy = policy;
        self
    }

    /// Returns the disk cache file of this fork at `block_number` on `chain_id`, when it caches on
    /// disk.
    pub fn cache_path(&self, chain_id: u64, block_number: u64) -> Option<PathBuf> {
        if !self.enable_caching || self.state_by_number {
            return None;
        }
        Some(self.cache_root.as_ref()?.join(chain_id.to_string()).join(block_number.to_string()))
    }

    /// Whether a new backend of this fork prefetches from an older cache file.
    ///
    /// Only a fork of the latest block starts cold on each new head; pinned blocks and rolls do
    /// not prefetch.
    const fn prefetches(&self) -> bool {
        self.prefetch && self.enable_caching && self.block.is_none() && !self.state_by_number
    }
}

/// A fork created by [`MultiFork::create_fork`] or [`MultiFork::roll_fork`].
#[derive(Clone, Debug)]
pub struct CreatedFork<N: Network = Ethereum> {
    /// The identifier of the fork.
    pub id: ForkId,
    /// The backend, pinned to the resolved fork block.
    pub backend: SharedBackend<N>,
    /// The environment of the resolved fork block.
    pub env: ForkEnv,
}

/// A cloneable handle to the background thread serving several forks.
///
/// Every method blocks until the thread answers, using [`BlockingMode::Auto`], so it works from
/// synchronous code and inside a multi-threaded Tokio runtime. When the last handle is dropped,
/// the thread flushes every cache and stops prefetching; backends still held keep working until
/// they are dropped too.
pub struct MultiFork<N: Network = Ethereum> {
    requests: UnboundedSender<Request<N>>,
    _shutdown: Arc<ShutDown<N>>,
}

impl<N: Network> Clone for MultiFork<N> {
    fn clone(&self) -> Self {
        Self { requests: self.requests.clone(), _shutdown: Arc::clone(&self._shutdown) }
    }
}

impl<N: Network> fmt::Debug for MultiFork<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultiFork").finish_non_exhaustive()
    }
}

impl<N: Network> MultiFork<N> {
    /// Spawns the background thread, connecting to endpoints over HTTP with rate limit retries.
    ///
    /// # Panics
    ///
    /// Panics if the thread or its runtime cannot be created.
    pub fn spawn() -> Self {
        Self::spawn_with_connector(http_provider::<N>)
    }

    /// Spawns the background thread, connecting to endpoints with `connector`.
    ///
    /// `connector` is called on the background thread with a fork's URL each time a fork is
    /// created or rolled.
    ///
    /// # Panics
    ///
    /// Panics if the thread or its runtime cannot be created.
    pub fn spawn_with_connector(
        connector: impl Fn(&str) -> ForkResult<DynProvider<N>> + Send + Sync + 'static,
    ) -> Self {
        let (requests, incoming) = unbounded_channel();
        let handler = MultiForkHandler::new(Arc::new(connector), incoming);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create multi fork runtime");
        thread::Builder::new()
            .name("evm2-multi-fork".into())
            .spawn(move || runtime.block_on(handler.run()))
            .expect("failed to spawn multi fork thread");
        let shutdown = ShutDown { requests: requests.clone() };
        Self { requests, _shutdown: Arc::new(shutdown) }
    }

    /// Creates a fork, reusing the backend of an existing fork of the same endpoint and block.
    ///
    /// A reused backend gets a distinct identifier with a `-N` suffix.
    pub fn create_fork(&self, fork: CreateFork) -> ForkResult<CreatedFork<N>> {
        self.request(|sender| Request::Create(Box::new(fork), sender))
            .unwrap_or(Err(ForkError::BackendShutdown))
    }

    /// Creates a fork from the options of fork `id`, at `block`.
    pub fn roll_fork(&self, id: &ForkId, block: u64) -> ForkResult<CreatedFork<N>> {
        self.request(|sender| Request::Roll(id.clone(), block, sender))
            .unwrap_or(Err(ForkError::BackendShutdown))
    }

    /// Returns the backend of fork `id`.
    pub fn get_fork(&self, id: &ForkId) -> Option<SharedBackend<N>> {
        self.request(|sender| Request::GetFork(id.clone(), sender)).flatten()
    }

    /// Returns the environment of fork `id`.
    pub fn get_env(&self, id: &ForkId) -> Option<ForkEnv> {
        self.request(|sender| Request::GetEnv(id.clone(), sender)).flatten()
    }

    /// Returns the endpoint of fork `id`.
    pub fn get_fork_url(&self, id: &ForkId) -> Option<String> {
        self.request(|sender| Request::GetForkUrl(id.clone(), sender)).flatten()
    }

    /// Returns the options fork `id` was created with.
    pub fn get_fork_options(&self, id: &ForkId) -> Option<CreateFork> {
        self.request(|sender| Request::GetForkOptions(id.clone(), sender)).flatten()
    }

    /// Flushes every fork's cache to disk each `period`, starting one `period` from now.
    ///
    /// Defaults to every 60 seconds.
    pub fn set_flush_cache_interval(&self, period: Duration) {
        let _ = self.requests.send(Request::SetFlushCacheInterval(period));
    }

    fn request<T>(&self, request: impl FnOnce(Sender<T>) -> Request<N>) -> Option<T> {
        BlockingMode::Auto.run(|| {
            let (sender, receiver) = mpsc::channel();
            self.requests.send(request(sender)).ok()?;
            receiver.recv().ok()
        })
    }
}

/// Connects to `url` over HTTP, retrying rate limited requests with backoff.
pub fn http_provider<N: Network>(url: &str) -> ForkResult<DynProvider<N>> {
    let connect_error = |err| ForkError::Connect(Arc::new(err));
    let BuiltInConnectionString::Http(url) =
        BuiltInConnectionString::try_as_http(url).map_err(connect_error)?
    else {
        return Err(connect_error(TransportErrorKind::custom_str("expected an HTTP URL")));
    };
    let retry = RetryBackoffLayer::new(
        MAX_RATE_LIMIT_RETRIES,
        INITIAL_BACKOFF_MS,
        COMPUTE_UNITS_PER_SECOND,
    );
    let client = RpcClient::builder().layer(retry).http(url);
    Ok(RootProvider::<N>::new(client).erased())
}

/// Shuts the handler down once the last [`MultiFork`] is dropped.
struct ShutDown<N: Network> {
    requests: UnboundedSender<Request<N>>,
}

impl<N: Network> Drop for ShutDown<N> {
    fn drop(&mut self) {
        let (sender, receiver) = mpsc::channel();
        if self.requests.send(Request::ShutDown(sender)).is_ok() {
            BlockingMode::Auto.run(|| {
                let _ = receiver.recv();
            });
        }
    }
}

/// A request from a [`MultiFork`] to its handler.
enum Request<N: Network> {
    Create(Box<CreateFork>, CreateSender<N>),
    Roll(ForkId, u64, CreateSender<N>),
    GetFork(ForkId, Sender<Option<SharedBackend<N>>>),
    GetEnv(ForkId, Sender<Option<ForkEnv>>),
    GetForkUrl(ForkId, Sender<Option<String>>),
    GetForkOptions(ForkId, Sender<Option<CreateFork>>),
    SetFlushCacheInterval(Duration),
    ShutDown(Sender<()>),
}

/// A fork block resolved through the endpoint.
struct Resolved<N: Network> {
    provider: DynProvider<N>,
    env: ForkEnv,
    bal: Option<BlockAccessList>,
}

/// A fork creation waiting for its block to resolve.
struct PendingCreate<N: Network> {
    opts: CreateFork,
    senders: Vec<CreateSender<N>>,
}

/// A fork known to the handler; forks sharing a backend share `num_senders`.
struct Entry<N: Network> {
    opts: CreateFork,
    env: ForkEnv,
    backend: SharedBackend<N>,
    num_senders: Arc<AtomicUsize>,
}

impl<N: Network> Clone for Entry<N> {
    fn clone(&self) -> Self {
        Self {
            opts: self.opts.clone(),
            env: self.env.clone(),
            backend: self.backend.clone(),
            num_senders: Arc::clone(&self.num_senders),
        }
    }
}

impl<N: Network> Entry<N> {
    /// Returns a new identifier for another fork sharing this backend.
    fn inc_senders(&self, id: &ForkId) -> ForkId {
        ForkId(format!("{id}-{}", self.num_senders.fetch_add(1, Ordering::Relaxed)))
    }
}

/// Serves [`MultiFork`] requests on the background thread.
struct MultiForkHandler<N: Network> {
    connector: Arc<Connector<N>>,
    incoming: UnboundedReceiver<Request<N>>,
    forks: HashMap<ForkId, Entry<N>>,
    /// The cache of every backend this handler created.
    caches: Vec<BlockchainDb>,
    /// Every backend's handler task.
    handlers: JoinSet<()>,
    pending: HashMap<u64, PendingCreate<N>>,
    creating: FuturesUnordered<CreateFuture<N>>,
    next_task: u64,
    flush_cache_interval: Option<Interval>,
    prefetches: Vec<Prefetch>,
}

impl<N: Network> MultiForkHandler<N> {
    fn new(connector: Arc<Connector<N>>, incoming: UnboundedReceiver<Request<N>>) -> Self {
        Self {
            connector,
            incoming,
            forks: Default::default(),
            caches: Vec::new(),
            handlers: JoinSet::new(),
            pending: Default::default(),
            creating: FuturesUnordered::new(),
            next_task: 0,
            flush_cache_interval: None,
            prefetches: Vec::new(),
        }
    }

    async fn run(mut self) {
        self.set_flush_cache_interval(DEFAULT_FLUSH_CACHE_INTERVAL);
        loop {
            tokio::select! {
                request = self.incoming.recv() => match request {
                    Some(Request::ShutDown(sender)) => {
                        self.shutdown().await;
                        let _ = sender.send(());
                        break;
                    }
                    Some(request) => self.on_request(request),
                    None => {
                        self.shutdown().await;
                        break;
                    }
                },
                Some((task, result)) = self.creating.next(), if !self.creating.is_empty() => {
                    self.on_created(task, result);
                }
                () = tick(&mut self.flush_cache_interval) => self.flush_caches(),
            }
        }
        // Keep serving backends that outlive the last `MultiFork` until they are dropped.
        while self.handlers.join_next().await.is_some() {}
    }

    fn on_request(&mut self, request: Request<N>) {
        match request {
            Request::Create(opts, sender) => self.create_fork(*opts, sender),
            Request::Roll(id, block, sender) => {
                let Some(fork) = self.forks.get(&id) else {
                    let _ = sender.send(Err(ForkError::UnknownFork(id.0)));
                    return;
                };
                let opts = fork.opts.clone().with_block(Some(block));
                self.create_fork(opts, sender);
            }
            Request::GetFork(id, sender) => {
                let _ = sender.send(self.forks.get(&id).map(|fork| fork.backend.clone()));
            }
            Request::GetEnv(id, sender) => {
                let _ = sender.send(self.forks.get(&id).map(|fork| fork.env.clone()));
            }
            Request::GetForkUrl(id, sender) => {
                let _ = sender.send(self.forks.get(&id).map(|fork| fork.opts.url.clone()));
            }
            Request::GetForkOptions(id, sender) => {
                let _ = sender.send(self.forks.get(&id).map(|fork| fork.opts.clone()));
            }
            Request::SetFlushCacheInterval(period) => self.set_flush_cache_interval(period),
            // Handled by `run`.
            Request::ShutDown(_) => {}
        }
    }

    fn create_fork(&mut self, opts: CreateFork, sender: CreateSender<N>) {
        // Only deduplicate explicit blocks: two `latest` requests can resolve to different blocks.
        if opts.block.is_some()
            && let Some(pending) = self.pending.values_mut().find(|pending| pending.opts == opts)
        {
            pending.senders.push(sender);
            return;
        }
        let provider = match (self.connector)(&opts.url) {
            Ok(provider) => provider,
            Err(err) => {
                let _ = sender.send(Err(err));
                return;
            }
        };
        let task = self.next_task;
        self.next_task += 1;
        let block = opts.block.map_or_else(BlockId::latest, BlockId::number);
        let prewarm_bal = opts.prewarm_bal;
        self.creating.push(Box::pin(async move {
            let result = async {
                let (env, block) = fork_env(&provider, block).await?;
                let bal = if prewarm_bal { bal::fetch_bal(&provider, &block).await } else { None };
                Ok(Resolved { provider, env, bal })
            };
            (task, result.await)
        }));
        self.pending.insert(task, PendingCreate { opts, senders: vec![sender] });
    }

    fn on_created(&mut self, task: u64, result: ForkResult<Resolved<N>>) {
        let Some(PendingCreate { opts, senders }) = self.pending.remove(&task) else { return };
        let Resolved { provider, env, bal } = match result {
            Ok(resolved) => resolved,
            Err(err) => {
                for sender in senders {
                    let _ = sender.send(Err(err.clone()));
                }
                return;
            }
        };
        let mut id = ForkId::resolved(&opts.url, env.block_number, opts.state_by_number);
        // The block was replaced since the existing fork was created: keep both apart.
        if self.forks.get(&id).is_some_and(|fork| fork.env.block_hash != env.block_hash) {
            id = ForkId(format!("{id}#{}", env.block_hash));
        }
        // A backend is shared only by forks with the same caching and fetching options.
        if self.forks.get(&id).is_some_and(|fork| !fork.opts.shares_backend_with(&opts)) {
            let mut variant = 1;
            let base = id.clone();
            while self.forks.get(&id).is_some_and(|fork| !fork.opts.shares_backend_with(&opts)) {
                id = ForkId(format!("{base}~{variant}"));
                variant += 1;
            }
        }
        let entry = match self.forks.get(&id) {
            Some(fork) => fork.clone(),
            None => self.create_backend(&opts, &env, provider),
        };
        if let Some(bal) = &bal {
            bal::cache_bal(entry.backend.data(), bal);
        }
        for sender in senders {
            let fork_id =
                if self.forks.contains_key(&id) { entry.inc_senders(&id) } else { id.clone() };
            let fork = Entry { opts: opts.clone(), env: env.clone(), ..entry.clone() };
            self.forks.insert(fork_id.clone(), fork);
            let created =
                CreatedFork { id: fork_id, backend: entry.backend.clone(), env: env.clone() };
            let _ = sender.send(Ok(created));
        }
    }

    /// Creates the backend of a resolved fork and spawns its handler, loading its disk cache.
    fn create_backend(
        &mut self,
        opts: &CreateFork,
        env: &ForkEnv,
        provider: DynProvider<N>,
    ) -> Entry<N> {
        let meta = BlockchainDbMeta::new(env.chain_id, env.block_number)
            .with_url(&opts.url)
            .with_block_hash(env.block_hash)
            .with_account_fetch_policy(opts.account_fetch_policy);
        let db = BlockchainDb::new(meta, opts.cache_path(env.chain_id, env.block_number));
        let pin = if opts.state_by_number {
            BlockId::number(env.block_number)
        } else {
            BlockId::hash(env.block_hash)
        };
        let (backend, handler) = SharedBackend::new(provider, db.clone(), Some(pin));
        self.handlers.spawn(handler.run());
        if opts.prefetches()
            && let Some(root) = &opts.cache_root
        {
            let chain_dir = root.join(env.chain_id.to_string());
            self.prefetches.extend(prefetch::spawn(&backend, chain_dir, env.block_number));
        }
        self.caches.push(db);
        Entry {
            opts: opts.clone(),
            env: env.clone(),
            backend,
            num_senders: Arc::new(AtomicUsize::new(1)),
        }
    }

    fn set_flush_cache_interval(&mut self, period: Duration) {
        self.flush_cache_interval = Some(tokio::time::interval_at(Instant::now() + period, period));
    }

    /// Flushes every cache on a blocking thread, so the handler keeps serving.
    fn flush_caches(&self) {
        if self.caches.is_empty() {
            return;
        }
        let caches = self.caches.clone();
        tokio::task::spawn_blocking(move || flush_all(&caches));
    }

    /// Drops every fork, stops prefetching and flushes every cache.
    async fn shutdown(&mut self) {
        self.forks.clear();
        self.pending.clear();
        self.creating.clear();
        let prefetches = std::mem::take(&mut self.prefetches);
        let caches = std::mem::take(&mut self.caches);
        // Prefetch workers wait on backend handlers running on this thread, so wait off it.
        let _ = tokio::task::spawn_blocking(move || {
            for prefetch in prefetches {
                prefetch.stop();
            }
            flush_all(&caches);
        })
        .await;
    }
}

fn flush_all(caches: &[BlockchainDb]) {
    for cache in caches {
        if let Err(err) = cache.flush() {
            tracing::warn!(target: "evm2::fork", ?err, "failed to flush fork cache");
        }
    }
}

/// Waits for the next tick of `interval`, or forever without one.
async fn tick(interval: &mut Option<Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => pending().await,
    }
}
