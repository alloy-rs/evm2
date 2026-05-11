//! EVM environment types.

use crate::{BaseEvmTypes, EvmTypes};
use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, U256};

/// Transaction-global environment values visible to opcodes.
#[derive(Clone, Debug)]
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
}

impl<T> PartialEq for TxEnv<T>
where
    T: EvmTypes,
    T::TxEnvExt: PartialEq,
{
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
            && self.gas_price == other.gas_price
            && self.chain_id == other.chain_id
            && self.blob_hashes == other.blob_hashes
            && self.ext == other.ext
    }
}

impl<T> Eq for TxEnv<T>
where
    T: EvmTypes,
    T::TxEnvExt: Eq,
{
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
        }
    }
}

/// Block environment values visible to opcodes.
#[derive(derive_more::Debug)]
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
}

impl<T> PartialEq for BlockEnv<T>
where
    T: EvmTypes,
    T::BlockEnvExt: PartialEq,
{
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.number == other.number
            && self.beneficiary == other.beneficiary
            && self.timestamp == other.timestamp
            && self.gas_limit == other.gas_limit
            && self.basefee == other.basefee
            && self.difficulty == other.difficulty
            && self.prevrandao == other.prevrandao
            && self.blob_basefee == other.blob_basefee
            && self.slot_num == other.slot_num
            && self.ext == other.ext
    }
}

impl<T> Eq for BlockEnv<T>
where
    T: EvmTypes,
    T::BlockEnvExt: Eq,
{
}

impl<T: EvmTypes> Clone for BlockEnv<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypes> Copy for BlockEnv<T> {}

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
        }
    }
}
