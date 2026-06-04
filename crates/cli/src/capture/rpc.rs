use super::CaptureError;
use alloy_primitives::Bytes;
use alloy_provider::{Provider, RootProvider, ext::DebugApi};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::BlockNumberOrTag;
use alloy_rpc_types_trace::geth::{GethDebugTracingOptions, PreStateConfig};
use serde_json::Value;
use std::{borrow::Cow, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const RPC_INITIAL_BACKOFF_MS: u64 = 500;
const RPC_COMPUTE_UNITS_PER_SECOND: u64 = 330;

#[derive(Clone, Copy)]
pub(super) enum TraceMode {
    PreState,
    Diff,
}

#[derive(Clone)]
pub(super) struct RpcEndpoint {
    provider: RootProvider,
    permits: Arc<Semaphore>,
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
        })
    }

    pub(super) async fn raw_block(&self, block_number: u64) -> Result<Bytes, CaptureError> {
        self.call(|| self.provider.debug_get_raw_block(block_number.into())).await
    }

    pub(super) async fn trace_block(
        &self,
        block_number: u64,
        mode: TraceMode,
    ) -> Result<Vec<Value>, CaptureError> {
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
