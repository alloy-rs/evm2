//! EVM configuration.

use crate::{
    SpecId, VersionTables,
    evm::{InMemoryDB, precompile::PrecompileProvider},
    interpreter::instructions::table::{InstructionTable, InstructionTables},
    spec_to_generic,
    version::Version,
};
use core::marker::PhantomData;

/// Runtime EVM type family.
///
/// Defines the concrete host, database, transaction, precompile, runtime spec-id, and config
/// selector types used by an EVM instance. This is runtime wiring, not version behavior.
pub trait EvmTypes: Sized + 'static {
    /// Configuration selector used by this EVM.
    type ConfigSelector: EvmConfigSelector<Self>;

    /// Runtime specification ID accepted by this EVM.
    ///
    /// Custom EVMs may use their own ID type, but every value must map to a base `SpecId`.
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

/// Compile-time EVM version configuration.
///
/// Names a concrete version for monomorphized code. It exposes the runtime `Version` data and the
/// type-specific `VersionTables` needed to build dispatch tables.
pub trait EvmConfig<T: EvmTypes> {
    /// Active EVM version.
    const VERSION: Version;

    /// Active type-specific version tables.
    const VERSION_TABLES: &'static VersionTables<T>;

    /// Active base specification ID.
    #[inline]
    fn spec_id() -> SpecId {
        Self::VERSION.spec_id()
    }
}

/// Runtime EVM config selector.
///
/// Maps a runtime spec-id value accepted by `EvmTypes` to the `ExecutionConfig` that the EVM and
/// interpreter use. Custom selectors can choose different configs for runtime IDs that share the
/// same inherited base `SpecId`.
pub trait EvmConfigSelector<T: EvmTypes>: Sized {
    /// Concrete EVM configuration for a base specification ID.
    ///
    /// `BASE_SPEC_ID` is always a `crate::SpecId` discriminant, even when `T::SpecId` is a custom
    /// runtime ID type.
    type Config<const BASE_SPEC_ID: u8>: EvmConfig<T>;

    /// Returns the selected execution config for `spec_id`.
    fn execution_config(spec_id: T::SpecId) -> ExecutionConfig<T>;
}

/// Selected execution configuration.
///
/// Bundles the active runtime `Version` with the finalized instruction dispatch table selected for
/// an EVM instance. This is the data passed to the interpreter when it runs.
#[derive(derive_more::Debug)]
pub struct ExecutionConfig<T: EvmTypes> {
    pub(crate) version: Version,
    #[debug(skip)]
    pub(crate) instructions: &'static InstructionTable<T>,
}

impl<T: EvmTypes> Clone for ExecutionConfig<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypes> Copy for ExecutionConfig<T> {}

impl<T: EvmTypes> ExecutionConfig<T> {
    /// Creates an execution config for a concrete compile-time config.
    #[inline]
    pub const fn for_config<C: EvmConfig<T>>() -> Self {
        Self { version: C::VERSION, instructions: <T as InstructionTables<C>>::INSTRUCTIONS }
    }

    /// Creates an execution config for a base `SpecId` through selector `F`.
    ///
    /// The selector provides the concrete `Config<BASE_SPEC_ID>` used for the inherited base
    /// version.
    #[inline]
    pub const fn for_base_spec<F: EvmConfigSelector<T>>(base_spec_id: SpecId) -> Self {
        spec_to_generic!(base_spec_id, |BASE_SPEC_ID| {
            Self::for_config::<F::Config<BASE_SPEC_ID>>()
        })
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
    type ConfigSelector = BaseEvmConfigSelector;
    type SpecId = SpecId;
    type Tx = Tx;
    type Host = crate::evm::Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = crate::evm::precompile::NoPrecompiles;
}

/// Base EVM configuration for an inherited base specification ID.
///
/// `BASE_SPEC_ID` is the raw discriminant of `crate::SpecId`, not a custom runtime spec-id value.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfig<const BASE_SPEC_ID: u8>(());

impl<T: EvmTypes, const BASE_SPEC_ID: u8> EvmConfig<T> for BaseEvmConfig<BASE_SPEC_ID> {
    const VERSION: Version = Version::base(SpecId::try_from_u8(BASE_SPEC_ID).unwrap());
    const VERSION_TABLES: &'static VersionTables<T> = &VersionTables::<T>::base::<Self>();
}

/// Base EVM config selector.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfigSelector(());

impl<T: EvmTypes<SpecId = SpecId>> EvmConfigSelector<T> for BaseEvmConfigSelector {
    type Config<const BASE_SPEC_ID: u8> = BaseEvmConfig<BASE_SPEC_ID>;

    fn execution_config(spec_id: T::SpecId) -> ExecutionConfig<T> {
        ExecutionConfig::for_base_spec::<Self>(spec_id)
    }
}
