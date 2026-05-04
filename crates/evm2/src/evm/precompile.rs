//! Precompile dispatch interface.

use crate::interpreter::{Gas, InstrStop};
use alloy_primitives::{Address, Bytes};

/// Result returned by a precompile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrecompileOutput {
    /// Returned bytes.
    pub output: Bytes,
}

/// Precompile execution hook.
pub trait PrecompileProvider {
    /// Returns precompile addresses that should be warm at transaction start.
    fn warm_addresses(&self) -> &'static [Address] {
        &[]
    }

    /// Executes the precompile at `address`, if one is registered.
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, InstrStop>>;
}

/// Empty precompile provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoPrecompiles;

impl PrecompileProvider for NoPrecompiles {
    #[inline]
    fn warm_addresses(&self) -> &'static [Address] {
        &[]
    }

    #[inline]
    fn execute(
        &mut self,
        _address: Address,
        _input: &[u8],
        _gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        None
    }
}
