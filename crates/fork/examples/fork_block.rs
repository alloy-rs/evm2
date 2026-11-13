//! Replays a mainnet block over a fork of its parent and reports how long it took and how many
//! RPC requests it made, checking the block's gas used against its header.
//!
//! ```sh
//! ETH_RPC_URL=https://... BLOCK=<number> cargo run -p evm2-fork --release --example fork_block
//! ```

use alloy_consensus::{TxEip4844, transaction::Recovered};
use alloy_hardforks::EthereumChainHardforks;
use alloy_provider::{Provider, RootProvider, network::Ethereum};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::BlockId;
use alloy_transport::layers::RetryBackoffLayer;
use evm2::{
    BaseEvmTypes, Evm, Precompiles,
    block::{
        BlockExecutor,
        ethereum::{EthBlockExecutionCtx, EthBlockExecutor, EthBlockRules, EthReceiptBuilder},
    },
    ethereum::ethereum_tx_registry,
    evm::DbStats,
};
use evm2_fork::{BlockchainDb, BlockchainDbMeta, ForkEnv, ForkedDatabase, SharedBackend};
use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};
use tower::{Layer, Service};

/// Counts HTTP requests and the JSON-RPC calls they carry.
#[derive(Clone, Debug, Default)]
struct Counter {
    requests: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

impl<S> Layer<S> for Counter {
    type Service = Counting<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Counting { inner, counter: self.clone() }
    }
}

#[derive(Clone, Debug)]
struct Counting<S> {
    inner: S,
    counter: Counter,
}

impl<S: Service<alloy_json_rpc::RequestPacket>> Service<alloy_json_rpc::RequestPacket>
    for Counting<S>
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: alloy_json_rpc::RequestPacket) -> Self::Future {
        self.counter.requests.fetch_add(1, Ordering::Relaxed);
        self.counter.calls.fetch_add(request.len(), Ordering::Relaxed);
        self.inner.call(request)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rpc_url = std::env::var("ETH_RPC_URL")?;
    let counter = Counter::default();
    // Like Foundry, retry rate-limited requests with backoff.
    let client = RpcClient::builder()
        .layer(RetryBackoffLayer::new(10, 500, 330))
        .layer(counter.clone())
        .http(rpc_url.parse()?);
    let provider = RootProvider::<Ethereum>::new(client);
    let runtime = tokio::runtime::Runtime::new()?;

    let number = match std::env::var("BLOCK") {
        Ok(block) => block.parse()?,
        Err(_) => runtime.block_on(provider.get_block_number())? - 1,
    };
    let block = runtime
        .block_on(async { provider.get_block(BlockId::number(number)).full().await })?
        .ok_or("block not found")?;
    let chain_id = runtime.block_on(provider.get_chain_id())?;
    let env = ForkEnv::from_header(chain_id, &block.header);
    let parent = block.header.parent_hash;
    let transactions = block
        .transactions
        .clone()
        .into_transactions()
        .map(|tx| {
            let (envelope, signer) = tx.inner.into_parts();
            Recovered::new_unchecked(envelope.map_eip4844(TxEip4844::from), signer)
        })
        .collect::<Vec<_>>();
    println!(
        "block {number}: {} txs, {} gas, {:?}",
        transactions.len(),
        block.header.gas_used,
        env.spec_id
    );

    let cache_path = std::env::var("CACHE").ok().map(Into::into);
    let meta =
        BlockchainDbMeta::new(chain_id, number - 1).with_block_hash(parent).with_url(&rpc_url);
    let db = BlockchainDb::new(meta, cache_path);
    let cached = db.accounts_len();
    let backend = SharedBackend::spawn_backend_thread(provider, db, Some(BlockId::hash(parent)));
    let handle = backend.clone();
    let setup = (counter.requests.load(Ordering::Relaxed), counter.calls.load(Ordering::Relaxed));

    let start = Instant::now();
    if std::env::var("PREFETCH").as_deref() == Ok("block") {
        let prefetch = handle.prefetch_block(BlockId::hash(block.header.hash))?;
        println!("prefetched {prefetch:?} in {:?}", start.elapsed());
    }
    let evm = Evm::<'static, BaseEvmTypes>::new(
        env.spec_id,
        env.block,
        ethereum_tx_registry(env.spec_id),
        DbStats::new(ForkedDatabase::new(backend)),
        Precompiles::base(env.spec_id),
    );
    let ctx = EthBlockExecutionCtx {
        tx_count_hint: Some(transactions.len()),
        parent_hash: parent,
        parent_beacon_block_root: block.header.parent_beacon_block_root,
        ommers: Cow::Borrowed(&[]),
        withdrawals: block.withdrawals.as_ref().map(|withdrawals| Cow::Owned(withdrawals.to_vec())),
    };
    let rules = EthBlockRules::new(&EthereumChainHardforks::mainnet(), number, None);
    let mut executor = EthBlockExecutor::new(evm, ctx, rules, EthReceiptBuilder::default());
    executor.apply_pre_execution_changes()?;
    for tx in &transactions {
        executor.execute_transaction(tx)?;
    }
    let stats =
        executor.evm.database_as::<DbStats<ForkedDatabase>>().expect("fork database").counts();
    let output = executor.finish()?;
    let elapsed = start.elapsed();

    let requests = counter.requests.load(Ordering::Relaxed) - setup.0;
    let calls = counter.calls.load(Ordering::Relaxed) - setup.1;
    println!(
        "executed in {elapsed:?}: gas {} (header {}), {requests} HTTP requests carrying {calls} RPC calls, {cached} accounts cached before",
        output.result.gas_used, block.header.gas_used,
    );
    println!("database reads: {stats:?}");
    assert_eq!(output.result.gas_used, block.header.gas_used, "gas used mismatch");
    handle.flush_cache()?;
    Ok(())
}
