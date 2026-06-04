use super::CaptureError;
use alloy_primitives::Bytes;
use alloy_provider::{Provider, RootProvider, ext::DebugApi};
use alloy_rpc_types_eth::BlockNumberOrTag;
use alloy_rpc_types_trace::geth::{GethDebugTracingOptions, PreStateConfig};
use serde_json::Value;
use std::{borrow::Cow, time::Duration};
use tokio::time::sleep;

const MAX_RPC_ATTEMPTS: usize = 4;

#[derive(Clone, Copy)]
pub(super) enum TraceMode {
    PreState,
    Diff,
}

#[derive(Clone)]
pub(super) struct RpcEndpoint {
    provider: RootProvider,
}

impl RpcEndpoint {
    pub(super) fn parse(url: &str) -> Result<Self, CaptureError> {
        let url = url.parse().map_err(|_| CaptureError::InvalidRpcUrl(url.to_owned()))?;
        Ok(Self { provider: RootProvider::new_http(url) })
    }

    pub(super) async fn raw_block(&self, block_number: u64) -> Result<Bytes, CaptureError> {
        self.call_with_retries("debug_getRawBlock", || {
            self.provider.debug_get_raw_block(block_number.into())
        })
        .await
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
        self.call_with_retries("debug_traceBlockByNumber", || {
            self.provider.raw_request(
                Cow::Borrowed("debug_traceBlockByNumber"),
                (BlockNumberOrTag::Number(block_number), &options),
            )
        })
        .await
    }

    async fn call_with_retries<R, F, Fut>(
        &self,
        method: &str,
        mut call: F,
    ) -> Result<R, CaptureError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = alloy_provider::transport::TransportResult<R>>,
    {
        for attempt in 1..=MAX_RPC_ATTEMPTS {
            match call().await {
                Ok(response) => return Ok(response),
                Err(error) if attempt < MAX_RPC_ATTEMPTS => {
                    eprintln!(
                        "RPC {method} attempt {attempt}/{MAX_RPC_ATTEMPTS} failed: {error}; retrying"
                    );
                    sleep(Duration::from_millis(500 * attempt as u64)).await;
                }
                Err(error) => return Err(CaptureError::Transport(error)),
            }
        }
        unreachable!("attempt loop always returns")
    }
}
