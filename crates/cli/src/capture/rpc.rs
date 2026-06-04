use super::{CaptureError, parse};
use alloy_primitives::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::sleep;

const MAX_RPC_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RPC_ATTEMPTS: usize = 4;

#[derive(Clone, Copy)]
pub(super) enum TraceMode {
    PreState,
    Diff,
}

pub(super) fn hex_quantity(value: u64) -> String {
    format!("0x{value:x}")
}

#[derive(Clone)]
pub(super) struct RpcEndpoint {
    client: reqwest::Client,
    url: String,
}

impl RpcEndpoint {
    pub(super) fn parse(url: &str) -> Result<Self, CaptureError> {
        let client = reqwest::Client::builder().user_agent("evm2-cli/0.1").build()?;
        Ok(Self { client, url: url.to_owned() })
    }

    pub(super) async fn raw_block(&self, block_id: &str) -> Result<Bytes, CaptureError> {
        let value = self.call("debug_getRawBlock", json!([block_id])).await?;
        parse::parse_bytes(&value)
    }

    pub(super) async fn trace_block(
        &self,
        block_id: &str,
        mode: TraceMode,
    ) -> Result<Vec<Value>, CaptureError> {
        let tracer_config = match mode {
            TraceMode::PreState => json!({}),
            TraceMode::Diff => json!({ "diffMode": true }),
        };
        let traces = self
            .call(
                "debug_traceBlockByNumber",
                json!([
                    block_id,
                    {
                        "tracer": "prestateTracer",
                        "tracerConfig": tracer_config,
                        "timeout": "120s"
                    }
                ]),
            )
            .await?;
        traces.as_array().cloned().ok_or(CaptureError::InvalidTraceResult(
            "debug_traceBlockByNumber result was not an array",
        ))
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, CaptureError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response = self.call_with_retries(method, &request).await?;
        if let Some(error) = response.error {
            return Err(CaptureError::Rpc { method: method.to_owned(), error });
        }
        response.result.ok_or_else(|| CaptureError::MissingRpcResult(method.to_owned()))
    }

    async fn call_with_retries(
        &self,
        method: &str,
        request: &Value,
    ) -> Result<RpcResponse, CaptureError> {
        for attempt in 1..=MAX_RPC_ATTEMPTS {
            match self.send_request(request).await {
                Ok(response) => return Ok(response),
                Err(CaptureError::Http(error)) if attempt < MAX_RPC_ATTEMPTS => {
                    eprintln!(
                        "RPC {method} attempt {attempt}/{MAX_RPC_ATTEMPTS} failed: {error}; retrying"
                    );
                    sleep(Duration::from_millis(500 * attempt as u64)).await;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("attempt loop always returns")
    }

    async fn send_request(&self, request: &Value) -> Result<RpcResponse, CaptureError> {
        let response = self
            .client
            .post(&self.url)
            .json(request)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        if response.len() as u64 > MAX_RPC_RESPONSE_BYTES {
            return Err(CaptureError::RpcResponseTooLarge { size: response.len() });
        }
        serde_json::from_slice(&response).map_err(CaptureError::DecodeRpcJson)
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<Value>,
}
