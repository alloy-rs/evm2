//! EVM environment types.

use crate::{BaseEvmTypes, EvmTypes};
use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, U256};
use derive_where::derive_where;

/// Transaction-global environment values visible to opcodes.
#[derive_where(Clone, Debug, PartialEq, Eq; T::TxEnvExt)]
pub struct TxEnv<T: EvmTypes = BaseEvmTypes> {
    /// Transaction origin.
    pub origin: Address,
    /// Effective gas price.
    pub gas_price: U256,
    /// Chain ID.
    pub chain_id: U256,
    /// Transaction blob versioned hashes.
    pub blob_hashes: Vec<U256>,
    /// EVM type-specific extension data.
    pub ext: T::TxEnvExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypes> Default for TxEnv<T> {
    #[inline]
    fn default() -> Self {
        Self {
            origin: Address::ZERO,
            gas_price: U256::ZERO,
            chain_id: U256::ONE,
            blob_hashes: vec![],
            ext: T::TxEnvExt::default(),
            _non_exhaustive: (),
        }
    }
}

/// Block environment values visible to opcodes.
#[derive_where(Clone, Copy, Debug, PartialEq, Eq; T::BlockEnvExt)]
pub struct BlockEnv<T: EvmTypes = BaseEvmTypes> {
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
    /// EVM type-specific extension data.
    pub ext: T::BlockEnvExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypes> Default for BlockEnv<T> {
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
            ext: T::BlockEnvExt::default(),
            _non_exhaustive: (),
        }
    }
}
