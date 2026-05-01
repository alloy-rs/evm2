#![doc = include_str!("../README.md")]
#![cfg_attr(
    feature = "nightly",
    feature(explicit_tail_calls, rust_preserve_none_cc),
    allow(incomplete_features)
)]
#![allow(clippy::missing_const_for_fn)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate self as evm2;

extern crate alloc;

pub mod bytecode;
/// EVM environment types.
pub mod env;
pub mod interpreter;
pub mod registry;

mod once_lock;
