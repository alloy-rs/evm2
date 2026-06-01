//! Precompile dispatch interface.

use super::Evm;
use crate::{EvmTypes, PrecompileError, interpreter::GasTracker};
use alloc::{boxed::Box, vec::Vec};
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
pub trait PrecompileProvider<T: EvmTypes>: Any + Send {
    /// Returns precompile addresses that should be warm at transaction start.
    fn warm_addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    /// Returns whether `address` has a registered precompile.
    fn contains(&self, address: &Address) -> bool;

    /// Executes the precompile at `address`, if one is registered.
    fn execute(
        &mut self,
        evm: &mut Evm<T>,
        address: Address,
        input: &[u8],
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
}

impl<T: EvmTypes> PrecompileProvider<T> for Box<dyn PrecompileProvider<T>> {
    #[inline]
    fn warm_addresses(&self) -> Vec<Address> {
        self.as_ref().warm_addresses()
    }

    #[inline]
    fn contains(&self, address: &Address) -> bool {
        self.as_ref().contains(address)
    }

    #[inline]
    fn execute(
        &mut self,
        evm: &mut Evm<T>,
        address: Address,
        input: &[u8],
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        self.as_mut().execute(evm, address, input, gas)
    }
}

/// Empty precompile provider.
#[allow(missing_debug_implementations, missing_copy_implementations)]
#[derive(Default)]
pub struct NoPrecompiles(());

impl<T: EvmTypes> PrecompileProvider<T> for NoPrecompiles {
    #[inline]
    fn warm_addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    #[inline]
    fn contains(&self, _address: &Address) -> bool {
        false
    }

    #[inline]
    fn execute(
        &mut self,
        _evm: &mut Evm<T>,
        _address: Address,
        _input: &[u8],
        _gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        None
    }
}
