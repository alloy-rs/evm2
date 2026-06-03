//! Precompile dispatch interface.

use super::Evm;
use crate::{
    EvmTypes, PrecompileError,
    interpreter::{GasTracker, Message},
};
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
pub trait PrecompileProvider<T: EvmTypes>: Any {
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
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
}

impl<T: EvmTypes, P: PrecompileProvider<T> + ?Sized> PrecompileProvider<T> for Box<P> {
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
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        self.as_mut().execute(evm, message, gas)
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
        _message: &Message<T>,
        _gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        None
    }
}
