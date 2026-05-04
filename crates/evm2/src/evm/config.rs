//! EVM configuration.

use crate::{
    SpecId,
    evm::{InMemoryDB, precompile::PrecompileProvider},
    version::Version,
};
use core::marker::PhantomData;

/// EVM compile-time type configuration.
pub trait EvmTypes: Sized + 'static {
    /// Transaction type handled by this EVM.
    type Tx;

    /// Host type used by this EVM.
    type Host: crate::interpreter::Host + ?Sized;

    /// Database type used by this EVM.
    type Database: crate::evm::Database;

    /// Precompile provider used by this EVM.
    type Precompiles: PrecompileProvider;
}

/// EVM configuration.
pub trait EvmConfig {
    /// Active EVM version.
    const VERSION: &'static Version;

    /// Active hard fork specification.
    #[inline]
    fn spec_id() -> SpecId {
        Self::VERSION.spec_id()
    }
}

/// Base EVM types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BaseEvmTypes<Tx = ()>(PhantomData<fn() -> Tx>);

impl<Tx: 'static> EvmTypes for BaseEvmTypes<Tx> {
    type Tx = Tx;
    type Host = crate::evm::Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = crate::evm::precompile::NoPrecompiles;
}

/// Base EVM configuration for a specification ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BaseEvmConfig<const SPEC_ID: u8>;

impl<const SPEC_ID: u8> EvmConfig for BaseEvmConfig<SPEC_ID> {
    const VERSION: &'static Version = Version::base(SpecId::try_from_u8(SPEC_ID).unwrap());
}
