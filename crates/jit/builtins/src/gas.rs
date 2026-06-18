//! Gas calculation utilities.

use evm2::interpreter::gas_constants;

pub const VERYLOW: u64 = gas_constants::VERYLOW as u64;
pub const LOG: u64 = gas_constants::LOG as u64;
pub const LOGDATA: u64 = gas_constants::LOGDATA as u64;
pub const LOGTOPIC: u64 = gas_constants::LOGTOPIC as u64;
pub const KECCAK256: u64 = gas_constants::KECCAK256 as u64;
pub const KECCAK256WORD: u64 = gas_constants::KECCAK256WORD as u64;
pub const COPY: u64 = gas_constants::COPY as u64;
