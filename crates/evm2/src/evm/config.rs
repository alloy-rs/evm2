//! EVM configuration.

use crate::{
    evm::{InMemoryDB, precompile::PrecompileProvider},
    interpreter::SpecId,
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
pub trait EvmConfig: 'static {
    /// Active EVM version.
    const VERSION: &'static Version;

    /// Active hard fork specification.
    #[inline]
    fn spec_id() -> SpecId {
        Self::VERSION.spec_id()
    }
}

/// EVM configuration for a specification ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BaseEvmTypes<const SPEC_ID: u8 = { SpecId::OSAKA as u8 }, Tx = ()>(
    PhantomData<fn() -> Tx>,
);

impl<Tx: 'static, const SPEC_ID: u8> EvmTypes for BaseEvmTypes<SPEC_ID, Tx> {
    type Tx = Tx;
    type Host = crate::evm::Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = crate::evm::precompile::NoPrecompiles;
}

impl<Tx: 'static, const SPEC_ID: u8> EvmConfig for BaseEvmTypes<SPEC_ID, Tx> {
    const VERSION: &'static Version = &Version::new_base(SpecId::try_from_u8(SPEC_ID).unwrap());
}
