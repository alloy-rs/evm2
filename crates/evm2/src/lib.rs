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

pub mod bytecode;
pub mod env;
/// EVM host and transaction dispatcher.
pub mod evm;
pub mod interpreter;
pub mod registry;

mod once_lock;
