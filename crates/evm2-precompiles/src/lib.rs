#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unreachable_pub)]
#![allow(rustdoc::bare_urls)]
#![allow(clippy::manual_is_multiple_of, clippy::manual_repeat_n)]

#[macro_use]
extern crate alloc;

#[allow(missing_docs)]
pub mod blake2;
#[allow(missing_docs)]
pub mod bn128;
#[allow(missing_docs)]
pub mod hash;
#[allow(missing_docs)]
pub mod identity;
#[allow(missing_docs)]
pub mod modexp;
#[allow(missing_docs)]
pub mod secp256k1;

use alloc::vec::Vec;
use alloy_primitives::Bytes;
use core::fmt;

/// Compatibility module for the copied revm precompile implementations.
pub mod primitives {
    pub use alloy_primitives::{Bytes, U256};
}

/// Raw 160-bit address bytes used by the copied revm implementation.
pub type B160 = [u8; 20];
/// Raw 256-bit word bytes used by the copied revm implementation.
pub type B256 = [u8; 32];

/// A precompile operation result.
pub type PrecompileResult = Result<(u64, Vec<u8>), PrecompileError>;

/// Standard precompile function type.
pub type StandardPrecompileFn = fn(&[u8], u64) -> PrecompileResult;
/// Custom precompile function type.
pub type CustomPrecompileFn = fn(&[u8], u64) -> PrecompileResult;

/// Precompile execution error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrecompileError {
    /// Out of gas.
    OutOfGas,
    /// BLAKE2 input length is invalid.
    Blake2WrongLength,
    /// BLAKE2 final indicator flag is invalid.
    Blake2WrongFinalIndicatorFlag,
    /// Modexp exponent length overflow.
    ModexpExpOverflow,
    /// Modexp base length overflow.
    ModexpBaseOverflow,
    /// Modexp modulus length overflow.
    ModexpModOverflow,
    /// BN128 field point is not a member.
    Bn128FieldPointNotAMember,
    /// BN128 affine point creation failed.
    Bn128AffineGFailedToCreate,
    /// BN128 pair input length is invalid.
    Bn128PairLength,
}

/// Alias used by the copied revm implementation.
pub type Error = PrecompileError;

/// Precompile output with optional logs.
#[derive(Debug)]
pub struct PrecompileOutput {
    /// Gas cost.
    pub cost: u64,
    /// Returned bytes.
    pub output: Vec<u8>,
    /// Emitted logs.
    pub logs: Vec<Log>,
}

/// Precompile log output.
#[derive(Debug, Default)]
pub struct Log {
    /// Log address.
    pub address: B160,
    /// Log topics.
    pub topics: Vec<B256>,
    /// Log data.
    pub data: Bytes,
}

impl PrecompileOutput {
    /// Creates an output without logs.
    pub const fn without_logs(cost: u64, output: Vec<u8>) -> Self {
        Self { cost, output, logs: Vec::new() }
    }
}

/// Precompile function.
#[derive(Clone, Copy)]
pub enum Precompile {
    /// Standard precompile.
    Standard(StandardPrecompileFn),
    /// Custom precompile.
    Custom(CustomPrecompileFn),
}

impl fmt::Debug for Precompile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard(_) => f.write_str("Standard"),
            Self::Custom(_) => f.write_str("Custom"),
        }
    }
}

/// Precompile address/function pair.
#[derive(Clone, Copy, Debug)]
pub struct PrecompileAddress(B160, Precompile);

impl From<PrecompileAddress> for (B160, Precompile) {
    fn from(value: PrecompileAddress) -> Self {
        (value.0, value.1)
    }
}

/// Calculates linear precompile gas.
pub const fn calc_linear_cost_u32(len: usize, base: u64, word: u64) -> u64 {
    (len as u64).div_ceil(32) * word + base
}

/// const fn for making an address by concatenating bytes from a `u64`.
const fn u64_to_b160(x: u64) -> B160 {
    let x_bytes = x.to_be_bytes();
    [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, x_bytes[0], x_bytes[1], x_bytes[2], x_bytes[3],
        x_bytes[4], x_bytes[5], x_bytes[6], x_bytes[7],
    ]
}

/// Executes a precompile function.
pub fn execute(precompile: Precompile, input: &[u8], gas_limit: u64) -> PrecompileResult {
    match precompile {
        Precompile::Standard(f) | Precompile::Custom(f) => f(input, gas_limit),
    }
}
