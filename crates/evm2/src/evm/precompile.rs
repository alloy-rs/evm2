//! Precompile dispatch interface.

use super::{Evm, NonStaticAny};
use crate::{
    EvmTypes, PrecompileError,
    interpreter::{GasTracker, Message},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, Bytes};

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
pub trait PrecompileProvider<T: EvmTypes>: NonStaticAny {
    /// Returns precompile addresses.
    fn addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    /// Returns whether `address` has a registered precompile.
    fn contains(&self, address: &Address) -> bool;

    /// Executes the precompile at `address`, if one is registered.
    fn execute(
        &mut self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
}

#[inline]
pub(crate) fn boxed_precompile_provider<'a, T: EvmTypes>(
    precompiles: impl PrecompileProvider<T> + 'a,
) -> Box<dyn PrecompileProvider<T> + 'a> {
    Box::new(precompiles)
}

impl<'a, T: EvmTypes> core::ops::Deref for dyn PrecompileProvider<T> + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a, T: EvmTypes> core::ops::DerefMut for dyn PrecompileProvider<T> + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

impl<T: EvmTypes, P: PrecompileProvider<T> + ?Sized> PrecompileProvider<T> for Box<P> {
    #[inline]
    fn addresses(&self) -> Vec<Address> {
        self.as_ref().addresses()
    }

    #[inline]
    fn contains(&self, address: &Address) -> bool {
        self.as_ref().contains(address)
    }

    #[inline]
    fn execute(
        &mut self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        self.as_mut().execute(evm, message, gas)
    }
}

/// Empty precompile provider.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Default)]
pub struct NoPrecompiles(());

impl<T: EvmTypes> PrecompileProvider<T> for NoPrecompiles {
    #[inline]
    fn addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    #[inline]
    fn contains(&self, _address: &Address) -> bool {
        false
    }

    #[inline]
    fn execute(
        &mut self,
        _evm: &mut Evm<'_, T>,
        _message: &Message<T>,
        _gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        None
    }
}
