//! EVM environment types.

use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, U256};

/// Transaction-global environment values visible to opcodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxEnv {
    /// Transaction origin.
    pub origin: Address,
    /// Effective gas price.
    pub gas_price: U256,
    /// Chain ID.
    pub chain_id: U256,
    /// Transaction blob versioned hashes.
    pub blob_hashes: Vec<U256>,
}

impl Default for TxEnv {
    #[inline]
    fn default() -> Self {
        Self {
            origin: Address::ZERO,
            gas_price: U256::ZERO,
            chain_id: U256::ONE,
            blob_hashes: vec![],
        }
    }
}

/// Block environment values visible to opcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEnv {
    /// Block number.
    pub number: U256,
    /// Block beneficiary.
    pub beneficiary: Address,
    /// Block timestamp.
    pub timestamp: U256,
    /// Block gas limit.
    pub gas_limit: U256,
    /// Block base fee.
    pub basefee: U256,
    /// Pre-merge block difficulty.
    pub difficulty: U256,
    /// Post-merge randomness value.
    pub prevrandao: U256,
    /// Blob base fee.
    pub blob_basefee: U256,
    /// Beacon slot number.
    pub slot_num: U256,
}

impl Default for BlockEnv {
    #[inline]
    fn default() -> Self {
        Self {
            number: U256::ZERO,
            beneficiary: Address::ZERO,
            timestamp: U256::ONE,
            gas_limit: U256::from_limbs([u64::MAX, 0, 0, 0]),
            basefee: U256::ZERO,
            difficulty: U256::ZERO,
            prevrandao: U256::ZERO,
            blob_basefee: U256::ONE,
            slot_num: U256::ZERO,
        }
    }
}
