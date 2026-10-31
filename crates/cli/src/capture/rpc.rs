use super::{CaptureError, MainnetBlock};
use alloy_consensus::Header;
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::BlockNumberOrTag;
use alloy_rpc_types_trace::geth::{GethDebugTracingOptions, PreStateConfig};
use serde_json::value::RawValue;
use std::{borrow::Cow, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const RPC_INITIAL_BACKOFF_MS: u64 = 500;
const RPC_COMPUTE_UNITS_PER_SECOND: u64 = 330;

#[derive(Clone, Copy)]
pub(super) enum TraceMode {
    PreState,
    Diff,
}

pub(super) struct RpcAccount {
    pub(super) balance: U256,
    pub(super) nonce: u64,
    pub(super) code: Bytes,
}

#[derive(Clone)]
pub(super) struct RpcEndpoint {
    provider: RootProvider,
    permits: Arc<Semaphore>,
    max_concurrent_requests: usize,
}

impl RpcEndpoint {
    pub(super) fn parse(
        url: &str,
        max_concurrent_requests: usize,
        rpc_retries: u32,
    ) -> Result<Self, CaptureError> {
        let url = url.parse().map_err(|_| CaptureError::InvalidRpcUrl(url.to_owned()))?;
        let retry_layer = alloy_provider::transport::layers::RetryBackoffLayer::new(
            rpc_retries,
            RPC_INITIAL_BACKOFF_MS,
            RPC_COMPUTE_UNITS_PER_SECOND,
        );
        let client = RpcClient::builder().layer(retry_layer).http(url);
        Ok(Self {
            provider: RootProvider::new(client),
            permits: Arc::new(Semaphore::new(max_concurrent_requests)),
            max_concurrent_requests,
        })
    }

    pub(super) const fn max_concurrent_requests(&self) -> usize {
        self.max_concurrent_requests
    }

    pub(super) async fn block(&self, block_number: u64) -> Result<MainnetBlock, CaptureError> {
        let block = self
            .call(|| async {
                self.provider
                    .get_block_by_number(BlockNumberOrTag::Number(block_number))
                    .full()
                    .await
            })
            .await?;
        block.map(Into::into).ok_or(CaptureError::MissingBlock(block_number))
    }

    pub(super) async fn header(&self, block_number: u64) -> Result<Header, CaptureError> {
        let header = self
            .call(|| self.provider.get_header_by_number(BlockNumberOrTag::Number(block_number)))
            .await?;
        header.map(|header| header.inner).ok_or(CaptureError::MissingBlockHeader(block_number))
    }

    /// Returns the balance, nonce and code of an account after this block.
    pub(super) async fn account(
        &self,
        address: Address,
        block_number: u64,
    ) -> Result<RpcAccount, CaptureError> {
        let block = BlockNumberOrTag::Number(block_number);
        let (balance, nonce, code) = tokio::try_join!(
            self.call(|| async { self.provider.get_balance(address).block_id(block.into()).await }),
            self.call(|| async {
                self.provider.get_transaction_count(address).block_id(block.into()).await
            }),
            self.call(|| async { self.provider.get_code_at(address).block_id(block.into()).await }),
        )?;
        Ok(RpcAccount { balance, nonce, code })
    }

    pub(super) async fn block_hash(&self, block_number: u64) -> Result<B256, CaptureError> {
        let header = self
            .call(|| self.provider.get_header_by_number(BlockNumberOrTag::Number(block_number)))
            .await?;
        header.map(|header| header.hash).ok_or(CaptureError::MissingBlockHeader(block_number))
    }

    pub(super) async fn trace_block(
        &self,
        block_number: u64,
        mode: TraceMode,
    ) -> Result<Box<RawValue>, CaptureError> {
        let config = match mode {
            TraceMode::PreState => PreStateConfig::default(),
            TraceMode::Diff => PreStateConfig { diff_mode: Some(true), ..Default::default() },
        };
        let options =
            GethDebugTracingOptions::prestate_tracer(config).with_timeout(Duration::from_secs(120));
        self.call(|| {
            self.provider.raw_request(
                Cow::Borrowed("debug_traceBlockByNumber"),
                (BlockNumberOrTag::Number(block_number), &options),
            )
        })
        .await
    }

    async fn call<R, F, Fut>(&self, call: F) -> Result<R, CaptureError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = alloy_provider::transport::TransportResult<R>>,
    {
        let _permit = self.permits.acquire().await.expect("capture semaphore is not closed");
        call().await.map_err(CaptureError::Transport)
    }
}
