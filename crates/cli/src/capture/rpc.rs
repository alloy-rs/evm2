use super::{CaptureError, parse};
use alloy_primitives::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{thread, time::Duration};

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

pub(super) struct RpcEndpoint {
    url: String,
}

impl RpcEndpoint {
    pub(super) fn parse(url: &str) -> Result<Self, CaptureError> {
        Ok(Self { url: url.to_owned() })
    }

    pub(super) fn raw_block(&self, block_id: &str) -> Result<Bytes, CaptureError> {
        let value = self.call("debug_getRawBlock", json!([block_id]))?;
        parse::parse_bytes(&value)
    }

    pub(super) fn trace_block(
        &self,
        block_id: &str,
        mode: TraceMode,
    ) -> Result<Vec<Value>, CaptureError> {
        let tracer_config = match mode {
            TraceMode::PreState => json!({}),
            TraceMode::Diff => json!({ "diffMode": true }),
        };
        let traces = self.call(
            "debug_traceBlockByNumber",
            json!([
                block_id,
                {
                    "tracer": "prestateTracer",
                    "tracerConfig": tracer_config,
                    "timeout": "120s"
                }
            ]),
        )?;
        traces.as_array().cloned().ok_or(CaptureError::InvalidTraceResult(
            "debug_traceBlockByNumber result was not an array",
        ))
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, CaptureError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response = self.call_with_retries(method, &request)?;
        if let Some(error) = response.error {
            return Err(CaptureError::Rpc { method: method.to_owned(), error });
        }
        response.result.ok_or_else(|| CaptureError::MissingRpcResult(method.to_owned()))
    }

    fn call_with_retries(
        &self,
        method: &str,
        request: &Value,
    ) -> Result<RpcResponse, CaptureError> {
        for attempt in 1..=MAX_RPC_ATTEMPTS {
            match self.send_request(request) {
                Ok(response) => return Ok(response),
                Err(error) if attempt < MAX_RPC_ATTEMPTS => {
                    eprintln!(
                        "RPC {method} attempt {attempt}/{MAX_RPC_ATTEMPTS} failed: {error}; retrying"
                    );
                    thread::sleep(Duration::from_millis(500 * attempt as u64));
                }
                Err(error) => return Err(CaptureError::Http(error)),
            }
        }
        unreachable!("attempt loop always returns")
    }

    fn send_request(&self, request: &Value) -> Result<RpcResponse, ureq::Error> {
        let response = ureq::post(&self.url)
            .header("User-Agent", "evm2-cli/0.1")
            .header("Connection", "close")
            .send_json(request)?
            .body_mut()
            .with_config()
            .limit(MAX_RPC_RESPONSE_BYTES)
            .read_json::<RpcResponse>()?;
        Ok(response)
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<Value>,
}
