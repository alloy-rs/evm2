//! Precompile dispatch interface.

use super::{Evm, NonStaticAny};
use crate::{
    EvmTypesHost, PrecompileError,
    interpreter::{GasTracker, Message},
    precompiles::PrecompileId,
};
use alloc::{sync::Arc, vec::Vec};
use alloy_primitives::{Address, Bytes};
use auto_impl::auto_impl;
use core::cell::RefCell;

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

/// Reentrant precompile execution hook.
///
/// The EVM keeps a shared owner alive during each call. Mutable state must use interior
/// mutability, with state borrows released before recursive host calls. Providers that
/// do not need recursion can implement [`MutPrecompileProvider`] and use [`RefCell`].
#[auto_impl(&, &mut, Box)]
pub trait PrecompileProvider<T: EvmTypesHost>: NonStaticAny {
    /// Returns precompile addresses.
    fn addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    /// Returns precompile addresses and identifiers.
    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
        Vec::new()
    }

    /// Returns whether `address` has a registered precompile.
    fn contains(&self, address: &Address) -> bool;

    /// Executes the precompile at `address`, if one is registered.
    fn execute(
        &self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
}

/// Mutable precompile provider, adapted through [`RefCell`].
///
/// Recursive execution while this provider is borrowed panics. Implement
/// [`PrecompileProvider`] directly to release state borrows before recursion.
#[auto_impl(&mut, Box)]
pub trait MutPrecompileProvider<T: EvmTypesHost>: NonStaticAny {
    /// Returns precompile addresses.
    fn addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    /// Returns precompile addresses and identifiers.
    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
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

impl<T: EvmTypesHost, P: MutPrecompileProvider<T>> PrecompileProvider<T> for RefCell<P> {
    fn addresses(&self) -> Vec<Address> {
        self.borrow().addresses()
    }

    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
        self.borrow().precompile_ids()
    }

    fn contains(&self, address: &Address) -> bool {
        self.borrow().contains(address)
    }

    fn execute(
        &self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        self.borrow_mut().execute(evm, message, gas)
    }
}

#[inline]
pub(crate) fn shared_precompile_provider<'a, T: EvmTypesHost>(
    precompiles: impl PrecompileProvider<T> + 'a,
) -> Arc<dyn PrecompileProvider<T> + 'a> {
    Arc::new(precompiles)
}

impl<'a, T: EvmTypesHost> core::ops::Deref for dyn PrecompileProvider<T> + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a, T: EvmTypesHost> core::ops::DerefMut for dyn PrecompileProvider<T> + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

/// Empty precompile provider.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Default)]
pub struct NoPrecompiles(());

impl<T: EvmTypesHost> PrecompileProvider<T> for NoPrecompiles {
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
        &self,
        _evm: &mut Evm<'_, T>,
        _message: &Message<T>,
        _gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        None
    }
}
