//! EVM transaction execution.

use crate::interpreter::{InstrStop, SpecId};
use alloy_primitives::{Address, Bytes, U256};
use thiserror::Error;

/// Transaction executed by the e2e EVM.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transaction {
    /// Transaction sender.
    pub caller: Address,
    /// Call destination. `None` means contract creation.
    pub to: Option<Address>,
    /// Sender nonce.
    pub nonce: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Effective gas price.
    pub gas_price: U256,
    /// Transferred value.
    pub value: U256,
    /// Input data or initcode.
    pub data: Bytes,
}

/// EVM execution result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutionResult {
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Gas used by the transaction after refunds.
    pub gas_used: u64,
    /// Return or revert output.
    pub output: Bytes,
}

impl ExecutionResult {
    /// Returns whether execution committed state changes.
    #[inline]
    pub const fn is_success(&self) -> bool {
        self.stop.is_success()
    }
}

/// EVM transaction validation or execution error.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum EvmError {
    /// Sender account does not have the expected nonce.
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce {
        /// Expected nonce.
        expected: u64,
        /// Transaction nonce.
        got: u64,
    },
    /// Transaction gas limit is lower than intrinsic gas.
    #[error("intrinsic gas too low: required {required}, got {got}")]
    IntrinsicGasTooLow {
        /// Required intrinsic gas.
        required: u64,
        /// Transaction gas limit.
        got: u64,
    },
    /// Sender cannot pay value plus maximum gas cost.
    #[error("insufficient funds")]
    InsufficientFunds,
}

/// Calculates intrinsic transaction gas.
pub fn intrinsic_gas(spec: SpecId, tx: &Transaction) -> u64 {
    let non_zero_multiplier = if spec.enables(SpecId::ISTANBUL) { 16 } else { 68 };
    let mut gas = 21_000;
    for byte in &tx.data {
        gas += if *byte == 0 { 4 } else { non_zero_multiplier };
    }
    if tx.to.is_none() && spec.enables(SpecId::HOMESTEAD) {
        gas += 32_000;
    }
    gas
}
