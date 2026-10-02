//! Precompile dispatch interface.

use super::{Evm, NonStaticAny};
use crate::{
    EvmTypesHost, PrecompileError,
    interpreter::{GasTracker, Message},
    precompiles::{MovePrecompileError, PrecompileId},
};
use alloc::{sync::Arc, vec::Vec};
use alloy_primitives::{Address, Bytes};
use auto_impl::auto_impl;

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
#[auto_impl(&mut, Box)]
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

    /// Returns whether this call message should enter native precompile dispatch.
    ///
    /// Implementations that share an address with bytecode can inspect the selector,
    /// call kind, and target here. A rejected message executes its bytecode. This
    /// method must not mutate state: it can be evaluated before a frame is run.
    #[inline]
    fn contains_message(&self, message: &Message<T>) -> bool {
        self.contains(&message.code_address)
    }

    /// Relocates installed precompiles, validating all sources before changing the provider.
    ///
    /// Entries whose source and destination match are ignored. Wrappers must preserve the
    /// underlying provider and invalidate any address-dependent caches after a successful move.
    fn move_precompiles(&mut self, moves: &[(Address, Address)])
    -> Result<(), MovePrecompileError>;

    /// Executes the precompile at `address`, if one is registered.
    ///
    /// Execution uses a shared provider reference so a child call can reenter
    /// dispatch. Execution state belongs in the EVM journal or in scoped interior
    /// mutability; do not hold an exclusive interior borrow across a child call.
    /// Provider configuration may only be mutated while EVM execution is idle.
    fn execute(
        &self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
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

    fn move_precompiles(
        &mut self,
        moves: &[(Address, Address)],
    ) -> Result<(), MovePrecompileError> {
        match moves.iter().find(|(source, dest)| source != dest) {
            Some((source, _)) => Err(MovePrecompileError::NotAPrecompile(*source)),
            None => Ok(()),
        }
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
