//! Precompile dispatch interface.

use crate::{PrecompileError, interpreter::Gas};
use alloc::vec::Vec;
use alloy_primitives::{Address, Bytes};
use core::any::Any;

/// Result returned by a precompile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PrecompileOutput {
    /// Returned bytes.
    bytes: Bytes,
}

impl PrecompileOutput {
    /// Creates a new precompile output.
    #[inline]
    pub const fn new(bytes: Bytes) -> Self {
        Self { bytes }
    }

    /// Returns the output bytes.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }

    /// Consumes the output and returns its bytes.
    #[inline]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }
}

/// Precompile execution hook.
pub trait PrecompileProvider: Any {
    /// Returns precompile addresses that should be warm at transaction start.
    fn warm_addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    /// Executes the precompile at `address`, if one is registered.
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
}

/// Empty precompile provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoPrecompiles;

impl PrecompileProvider for NoPrecompiles {
    #[inline]
    fn warm_addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    #[inline]
    fn execute(
        &mut self,
        _address: Address,
        _input: &[u8],
        _gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        None
    }
}
