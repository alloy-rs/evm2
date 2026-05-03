//! Precompile dispatch interface.

use crate::interpreter::InstrStop;
use alloy_primitives::{Address, Bytes};

/// Result returned by a precompile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrecompileOutput {
    /// Gas used by the precompile.
    pub gas_used: u64,
    /// Returned bytes.
    pub output: Bytes,
}

/// Precompile execution hook.
pub trait PrecompileProvider {
    /// Executes the precompile at `address`, if one is registered.
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas_limit: u64,
    ) -> Option<Result<PrecompileOutput, InstrStop>>;
}

/// Empty precompile provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoPrecompiles;

impl PrecompileProvider for NoPrecompiles {
    #[inline]
    fn execute(
        &mut self,
        _address: Address,
        _input: &[u8],
        _gas_limit: u64,
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        None
    }
}
