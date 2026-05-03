//! Utility functions for precompile addresses and gas costs.

use crate::Address;

/// Calculate the linear cost of a precompile.
#[inline]
pub const fn calc_linear_cost(len: usize, base: u64, word: u64) -> u64 {
    (len as u64).div_ceil(32) * word + base
}

/// Calculate the linear cost of a precompile.
#[deprecated(note = "please use `calc_linear_cost` instead")]
pub const fn calc_linear_cost_u32(len: usize, base: u64, word: u64) -> u64 {
    calc_linear_cost(len, base, word)
}

/// Const function for making an address by concatenating the bytes from two given numbers.
///
/// Note that 32 + 128 = 160 = 20 bytes (the length of an address).
///
/// This function is used as a convenience for specifying the addresses of the various precompiles.
#[inline]
pub const fn u64_to_address(x: u64) -> Address {
    let x = x.to_be_bytes();
    Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7],
    ])
}
