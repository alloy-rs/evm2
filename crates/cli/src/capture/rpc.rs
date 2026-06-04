use super::{CaptureError, parse};
use alloy_primitives::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};

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
        let response = ureq::post(&self.url)
            .send_json(&request)
            .map_err(CaptureError::Http)?
            .body_mut()
            .read_json::<RpcResponse>()
            .map_err(CaptureError::Http)?;
        if let Some(error) = response.error {
            return Err(CaptureError::Rpc { method: method.to_owned(), error });
        }
        response.result.ok_or_else(|| CaptureError::MissingRpcResult(method.to_owned()))
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<Value>,
}
