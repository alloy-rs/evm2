#![doc = include_str!("../README.md")]
#![cfg_attr(
    feature = "nightly",
    feature(explicit_tail_calls, rust_preserve_none_cc),
    allow(incomplete_features)
)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate self as evm2;

extern crate alloc;

use alloy_primitives::{B256, Bytes};

/// Loaded account information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccountLoad {
    /// Account balance.
    pub balance: interpreter::Word,
    /// Account code hash.
    pub code_hash: B256,
    /// Account code bytes.
    pub code: Bytes,
    /// Whether the account is empty.
    pub is_empty: bool,
    /// Whether the account access was cold.
    pub is_cold: bool,
}

/// Loaded storage slot value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StorageLoad {
    /// Storage slot value.
    pub value: interpreter::Word,
    /// Whether the storage slot access was cold.
    pub is_cold: bool,
}

/// Result of a `SELFDESTRUCT` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SelfDestructResult {
    /// Whether the destroyed account had non-zero value.
    pub had_value: bool,
    /// Whether the beneficiary account already exists.
    pub target_exists: bool,
    /// Whether the beneficiary access was cold.
    pub is_cold: bool,
    /// Whether this account was already destroyed in this transaction.
    pub previously_destroyed: bool,
}

pub mod bytecode;
/// Ethereum transaction types and handlers.
pub mod ethereum;
/// EVM host and transaction dispatcher.
pub mod evm;
pub mod interpreter;
pub mod version;

pub use evm::{
    Evm, TxResult, config,
    config::{BaseEvmTypes, EvmConfig, EvmTypes},
    env, precompile, registry,
};
pub use version::{EvmVersion, Version};

mod once_lock;

#[cfg(test)]
mod tests;

/// Exposes a small interpreter run for assembly inspection.
#[unsafe(no_mangle)]
#[doc(hidden)]
pub fn _get_asm() -> impl Sized {
    let mut evm = Evm::<BaseEvmTypes>::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    crate::interpreter::Interpreter::new(Default::default(), Default::default(), Default::default())
        .run::<BaseEvmTypes, BaseEvmTypes>(&mut evm)
}
