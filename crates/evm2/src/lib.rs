#![doc = include_str!("../README.md")]
#![cfg_attr(tco, feature(explicit_tail_calls, rust_preserve_none_cc), allow(incomplete_features))]
#![cfg_attr(not(feature = "std"), no_std)]
#![warn(unused_crate_dependencies)]

extern crate self as evm2;

extern crate alloc;

#[cfg(test)]
use {ark_std as _, criterion as _, rand as _, revm as _, serde as _, serde_json as _};

pub mod bytecode;
pub(crate) mod constants;
pub mod ethereum;
pub mod interpreter;
pub mod utils;

pub mod evm;
pub use evm::{
    BEACON_ROOTS_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, Evm, HISTORY_STORAGE_ADDRESS,
    SYSTEM_ADDRESS, SYSTEM_CALL_GAS_LIMIT, TxResult, WITHDRAWAL_REQUEST_ADDRESS, config,
    config::{
        BaseEvmConfig, BaseEvmConfigSelector, BaseEvmTypes, EvmConfig, EvmConfigSelector, EvmTypes,
        ExecutionConfig,
    },
    env, inspector, precompile, registry,
};

pub mod precompiles;
pub use precompiles::{
    Crypto, PrecompileError, PrecompileHalt, Precompiles, crypto, install_crypto,
};

pub(crate) mod trustme;

pub mod version;
pub use version::{EvmFeatures, OpcodeConfig, Version};

mod spec_id;
pub use spec_id::SpecId;

mod once_lock;
mod storage_key;
pub use storage_key::{StorageKey, StorageKeyMap, StorageKeySet};

#[cfg(test)]
mod tests;
