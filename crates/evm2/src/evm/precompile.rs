//! Precompile dispatch interface.

use crate::{PrecompileError, interpreter::Gas};
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

impl dyn PrecompileProvider {
    /// Returns `true` if the precompile provider has type `T`.
    #[inline]
    pub fn is<T: PrecompileProvider>(&self) -> bool {
        (self as &dyn Any).is::<T>()
    }

    /// Returns the concrete precompile provider if it has type `T`.
    #[inline]
    pub fn downcast_ref<T: PrecompileProvider>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }

    /// Returns the concrete precompile provider mutably if it has type `T`.
    #[inline]
    pub fn downcast_mut<T: PrecompileProvider>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut()
    }
}

impl<T: PrecompileProvider> From<Box<T>> for Box<dyn PrecompileProvider> {
    #[inline]
    fn from(value: Box<T>) -> Self {
        value
    }
}

impl PrecompileProvider for Box<dyn PrecompileProvider> {
    #[inline]
    fn warm_addresses(&self) -> Vec<Address> {
        self.as_ref().warm_addresses()
    }

    #[inline]
    fn execute(
        &mut self,
        address: Address,
        input: &[u8],
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        self.as_mut().execute(address, input, gas)
    }
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
