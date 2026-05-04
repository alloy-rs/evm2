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
/// Ethereum transaction types and handlers.
pub mod ethereum;
/// EVM host and transaction dispatcher.
pub mod evm;
pub mod interpreter;
pub(crate) mod precompiles;

pub use evm::{
    Evm, TxResult, config,
    config::{EvmConfig, EvmVersion},
    env, precompile, registry,
};
pub use precompiles::{Crypto, PrecompileHalt, crypto, install_crypto};

mod once_lock;

#[cfg(test)]
mod tests;

/// Exposes a small interpreter run for assembly inspection.
#[unsafe(no_mangle)]
#[doc(hidden)]
pub fn _get_asm() -> impl Sized {
    let mut evm = Evm::<EvmVersion<()>>::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    crate::interpreter::Interpreter::new(Default::default(), Default::default(), Default::default())
        .run::<EvmVersion<()>>(&mut evm)
}
