#![doc = include_str!("../README.md")]
#![cfg_attr(
    feature = "nightly",
    feature(explicit_tail_calls, rust_preserve_none_cc),
    allow(incomplete_features)
)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate self as evm2;

extern crate alloc;

pub mod bytecode;
pub mod ethereum;
pub mod interpreter;
pub mod utils;
pub mod version;

pub mod evm;
pub use evm::{
    Evm, TxResult, config,
    config::{
        BaseEvmConfig, BaseEvmConfigSelector, BaseEvmTypes, EvmConfig, EvmConfigSelector, EvmTypes,
        ExecutionConfig,
    },
    env, precompile, registry,
};

pub(crate) mod precompiles;
pub use precompiles::{Crypto, PrecompileHalt, Precompiles, crypto, install_crypto};

pub use version::{Version, VersionTables};

mod spec_id;
pub use spec_id::SpecId;

mod once_lock;

#[cfg(test)]
mod tests;
