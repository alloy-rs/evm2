use super::CaptureError;
use alloy_primitives::{Address, B256, Bytes, U256, hex};
use serde_json::Value;
use std::str::FromStr;

pub(super) fn trace_result(trace: &Value) -> Result<&Value, CaptureError> {
    trace.get("result").ok_or(CaptureError::InvalidTraceResult("trace item did not contain result"))
}

pub(super) fn parse_address(value: &str) -> Result<Address, CaptureError> {
    Address::from_str(value).map_err(|_| CaptureError::InvalidHex(value.to_owned()))
}

pub(super) fn parse_b256(value: &str) -> Result<B256, CaptureError> {
    B256::from_str(value).map_err(|_| CaptureError::InvalidHex(value.to_owned()))
}

pub(super) fn parse_bytes(value: &Value) -> Result<Bytes, CaptureError> {
    let value = value.as_str().ok_or(CaptureError::InvalidTraceResult("expected hex string"))?;
    let value =
        value.strip_prefix("0x").ok_or_else(|| CaptureError::InvalidHex(value.to_owned()))?;
    hex::decode(value).map(Bytes::from).map_err(|_| CaptureError::InvalidHex(format!("0x{value}")))
}

pub(super) fn parse_u256(value: &Value) -> Result<U256, CaptureError> {
    if let Some(value) = value.as_str() {
        let value = value.strip_prefix("0x").unwrap_or(value);
        return U256::from_str_radix(if value.is_empty() { "0" } else { value }, 16)
            .map_err(|_| CaptureError::InvalidNumber(format!("0x{value}")));
    }
    if let Some(value) = value.as_u64() {
        return Ok(U256::from(value));
    }
    Err(CaptureError::InvalidTraceResult("expected integer or hex quantity"))
}

pub(super) fn parse_u64(value: &Value) -> Result<u64, CaptureError> {
    if let Some(value) = value.as_u64() {
        return Ok(value);
    }
    if let Some(value) = value.as_str() {
        let value = value.strip_prefix("0x").unwrap_or(value);
        return u64::from_str_radix(if value.is_empty() { "0" } else { value }, 16)
            .map_err(|_| CaptureError::InvalidNumber(format!("0x{value}")));
    }
    Err(CaptureError::InvalidTraceResult("expected integer or hex quantity"))
}
