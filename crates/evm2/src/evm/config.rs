//! EVM configuration.

use crate::{
    EvmVersion, SpecId,
    evm::{InMemoryDB, precompile::PrecompileProvider},
    interpreter::{InstrStop, Interpreter},
    spec_to_generic,
    version::Version,
};
use core::marker::PhantomData;

/// EVM compile-time type configuration.
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

/// EVM runtime configuration.
pub trait EvmConfig {
    /// Active EVM version.
    const VERSION: &'static Version;

    /// Active hard fork specification.
    #[inline]
    fn spec_id() -> SpecId {
        Self::VERSION.spec_id()
    }
}

/// Factory for selecting an EVM configuration from a runtime specification ID.
pub trait EvmConfigFactory<T: EvmTypes>: Sized {
    /// Concrete EVM configuration for a base specification ID.
    type Config<const SPEC_ID: u8>: EvmConfig;

    /// Returns the active EVM version for `spec_id`.
    #[inline]
    fn version(spec_id: T::SpecId) -> &'static Version {
        Version::base(spec_id.into())
    }

    /// Returns the interpreter runner for `spec_id`.
    fn run_interpreter(spec_id: T::SpecId) -> crate::evm::RunInterpreterFn<T>;

    /// Returns the type-specific EVM version for `Cfg`.
    #[inline]
    fn evm_version<Cfg: EvmConfig>() -> &'static EvmVersion<T> {
        const { &EvmVersion::<T>::new_base::<Cfg>() }
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

impl<const SPEC_ID: u8> EvmConfig for BaseEvmConfig<SPEC_ID> {
    const VERSION: &'static Version = Version::base(SpecId::try_from_u8(SPEC_ID).unwrap());
}

/// Base EVM configuration factory.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfigFactory(());

impl<T: EvmTypes<SpecId = SpecId>> EvmConfigFactory<T> for BaseEvmConfigFactory {
    type Config<const SPEC_ID: u8> = BaseEvmConfig<SPEC_ID>;

    fn run_interpreter(spec_id: T::SpecId) -> crate::evm::RunInterpreterFn<T> {
        base_run_interpreter::<T, Self>(spec_id)
    }
}

/// Returns the interpreter runner for a base EVM specification.
pub fn base_run_interpreter<T, F>(spec_id: SpecId) -> crate::evm::RunInterpreterFn<T>
where
    T: EvmTypes,
    F: EvmConfigFactory<T>,
{
    spec_to_generic!(spec_id, |SPEC_ID| run_interpreter::<T, F::Config<SPEC_ID>>)
}

fn run_interpreter<T: EvmTypes, C: EvmConfig>(
    interpreter: &mut Interpreter<T>,
    host: &mut T::Host,
) -> InstrStop {
    interpreter.run::<C>(host)
}
