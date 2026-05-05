//! EVM configuration.

use crate::{
    EvmVersion, SpecId,
    evm::{InMemoryDB, precompile::PrecompileProvider},
    interpreter::table::{InstructionTable, InstructionTables},
    spec_to_generic,
    version::Version,
};
use core::marker::PhantomData;

/// EVM type configuration.
pub trait EvmTypes: Sized + 'static {
    /// Configuration factory used by this EVM.
    type ConfigFactory: EvmConfigFactory<Self>;

    /// Runtime specification ID accepted by this EVM.
    type SpecId: Copy + Into<SpecId>;

    /// Transaction type handled by this EVM.
    type Tx;

    /// Host type used by this EVM.
    type Host: crate::interpreter::Host + ?Sized;

    /// Database type used by this EVM.
    type Database: crate::evm::Database;

    /// Precompile provider used by this EVM.
    type Precompiles: PrecompileProvider;
}

/// EVM compile-time configuration.
pub trait EvmConfig<T: EvmTypes> {
    /// Active EVM version.
    const VERSION: Version;

    /// Active type-specific EVM version.
    const EVM_VERSION: &'static EvmVersion<T>;

    /// Active hard fork specification.
    #[inline]
    fn spec_id() -> SpecId {
        Self::VERSION.spec_id()
    }
}

/// Factory for selecting an EVM configuration (compile-time or runtime) from a runtime
/// specification ID.
pub trait EvmConfigFactory<T: EvmTypes>: Sized {
    /// Concrete EVM configuration for a base specification ID.
    type Config<const SPEC_ID: u8>: EvmConfig<T>;

    /// Returns the EVM runtime config for `spec_id`.
    fn evm_runtime_config(spec_id: T::SpecId) -> EvmRuntimeConfig<T>;
}

/// Runtime config selected for EVM execution.
#[derive(derive_more::Debug)]
pub struct EvmRuntimeConfig<T: EvmTypes> {
    pub(crate) version: Version,
    #[debug(skip)]
    pub(crate) instructions: &'static InstructionTable<T>,
}

impl<T: EvmTypes> Clone for EvmRuntimeConfig<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypes> Copy for EvmRuntimeConfig<T> {}

impl<T: EvmTypes> EvmRuntimeConfig<T> {
    /// Creates EVM runtime config for `C`.
    #[inline]
    pub const fn new<C: EvmConfig<T>>() -> Self {
        Self { version: C::VERSION, instructions: <T as InstructionTables<C>>::INSTRUCTIONS }
    }

    /// Returns the active EVM version.
    #[inline]
    pub const fn version(&self) -> &Version {
        &self.version
    }
}

/// Base EVM types.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmTypes<Tx = ()>(PhantomData<fn() -> Tx>);

impl<Tx: 'static> EvmTypes for BaseEvmTypes<Tx> {
    type ConfigFactory = BaseEvmConfigFactory;
    type SpecId = SpecId;
    type Tx = Tx;
    type Host = crate::evm::Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = crate::evm::precompile::NoPrecompiles;
}

/// Base EVM configuration for a specification ID.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfig<const SPEC_ID: u8>(());

impl<T: EvmTypes, const SPEC_ID: u8> EvmConfig<T> for BaseEvmConfig<SPEC_ID> {
    const VERSION: Version = Version::base(SpecId::try_from_u8(SPEC_ID).unwrap());
    const EVM_VERSION: &'static EvmVersion<T> = &EvmVersion::<T>::new_base::<Self>();
}

/// Base EVM configuration factory.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfigFactory(());

impl<T: EvmTypes<SpecId = SpecId>> EvmConfigFactory<T> for BaseEvmConfigFactory {
    type Config<const SPEC_ID: u8> = BaseEvmConfig<SPEC_ID>;

    fn evm_runtime_config(spec_id: T::SpecId) -> EvmRuntimeConfig<T> {
        base_evm_runtime_config::<T, Self>(spec_id)
    }
}

/// Returns EVM runtime config for a base EVM specification.
pub const fn base_evm_runtime_config<T, F>(spec_id: SpecId) -> EvmRuntimeConfig<T>
where
    T: EvmTypes,
    F: EvmConfigFactory<T>,
{
    spec_to_generic!(spec_id, |SPEC_ID| EvmRuntimeConfig::<T>::new::<F::Config<SPEC_ID>>())
}
