//! Precompile dispatch interface.

use crate::{
    PrecompileHalt,
    interpreter::{Gas, InstrStop},
};
use alloc::vec::Vec;
use alloy_primitives::{Address, Bytes};

/// Status of a precompile execution.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PrecompileStatus {
    /// Precompile executed successfully.
    Success,
    /// Precompile reverted.
    Revert,
    /// Precompile halted with a specific reason.
    Halt(PrecompileHalt),
}

/// Result returned by a precompile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PrecompileOutput {
    /// Precompile execution status.
    status: PrecompileStatus,
    /// Returned bytes.
    bytes: Bytes,
}

impl Default for PrecompileStatus {
    #[inline]
    fn default() -> Self {
        Self::Success
    }
}

impl PrecompileOutput {
    /// Creates a new precompile output.
    #[inline]
    pub const fn new(bytes: Bytes) -> Self {
        Self { status: PrecompileStatus::Success, bytes }
    }

    /// Creates a new reverted precompile output.
    #[inline]
    pub const fn revert(bytes: Bytes) -> Self {
        Self { status: PrecompileStatus::Revert, bytes }
    }

    /// Creates a new halted precompile output.
    #[inline]
    pub const fn halt(reason: PrecompileHalt) -> Self {
        Self { status: PrecompileStatus::Halt(reason), bytes: Bytes::new() }
    }

    /// Returns the precompile execution status.
    #[inline]
    pub const fn status(&self) -> &PrecompileStatus {
        &self.status
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

    /// Returns `true` if the precompile execution was successful.
    #[inline]
    pub const fn is_success(&self) -> bool {
        matches!(self.status, PrecompileStatus::Success)
    }

    /// Returns `true` if the precompile reverted.
    #[inline]
    pub const fn is_revert(&self) -> bool {
        matches!(self.status, PrecompileStatus::Revert)
    }

    /// Returns `true` if the precompile halted.
    #[inline]
    pub const fn is_halt(&self) -> bool {
        matches!(self.status, PrecompileStatus::Halt(_))
    }

    /// Returns the halt reason if the precompile halted, `None` otherwise.
    #[inline]
    pub const fn halt_reason(&self) -> Option<&PrecompileHalt> {
        match &self.status {
            PrecompileStatus::Halt(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Precompile execution hook.
pub trait PrecompileProvider {
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
    ) -> Option<Result<PrecompileOutput, InstrStop>>;
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
    ) -> Option<Result<PrecompileOutput, InstrStop>> {
        None
    }
}
