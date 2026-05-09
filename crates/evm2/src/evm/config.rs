//! EVM configuration.

use crate::{
    SpecId, VersionTables,
    ethereum::RecoveredTxEnvelope,
    interpreter::{Host, instructions::table::InstrTable},
    version::Version,
};

/// Runtime EVM type family.
///
/// Defines the concrete host, transaction, runtime spec-id, and config selector types used by an
/// EVM instance. This is runtime wiring, not version behavior.
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
    type Host: Host + ?Sized;
}

/// Compile-time EVM table configuration.
///
/// Names the inherited base spec and type-specific `VersionTables` needed to build dispatch tables.
pub trait EvmConfig<T: EvmTypes> {
    /// Inherited base specification ID.
    const BASE_SPEC_ID: SpecId;

    /// Active type-specific version tables.
    const VERSION_TABLES: &'static VersionTables<T>;
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

    /// Ordered version tables for this selector's base-spec config family.
    const VERSION_TABLES: &'static [&'static VersionTables<T>; SpecId::COUNT] =
        &selector_version_tables::<T, Self>();

    /// Returns the selected execution config for `spec_id`.
    fn execution_config(spec_id: T::SpecId) -> ExecutionConfig<T>;
}

#[doc(hidden)]
#[allow(private_interfaces)]
pub trait InstrTables<F>: EvmTypes
where
    F: EvmConfigSelector<Self>,
{
    const INSTRUCTIONS: &'static [InstrTable<Self>; SpecId::COUNT];
    const INSPECT_INSTRUCTIONS: &'static [InstrTable<Self>; SpecId::COUNT];
}

const fn selector_version_tables<T, F>() -> [&'static VersionTables<T>; SpecId::COUNT]
where
    T: EvmTypes,
    F: EvmConfigSelector<T>,
{
    macro_rules! version_tables {
        ([$evm_types:ty, $selector:ty] $($spec:ident $name:ident,)*) => {
            [
                $(
                    <<$selector as EvmConfigSelector<$evm_types>>::Config<{ SpecId::$spec as u8 }>
                        as EvmConfig<$evm_types>>::VERSION_TABLES,
                )*
            ]
        };
    }

    crate::for_each_spec!([T, F] version_tables)
}

/// Selected execution configuration.
///
/// Bundles the active runtime `Version` with the finalized instruction dispatch table selected for
/// an EVM instance. This is the data passed to the interpreter when it runs.
#[derive(derive_more::Debug)]
pub struct ExecutionConfig<T: EvmTypes> {
    pub(crate) version: Version,
    #[debug(skip)]
    pub(crate) instructions: &'static InstrTable<T>,
    #[debug(skip)]
    pub(crate) inspect_instructions: &'static InstrTable<T>,
}

impl<T: EvmTypes> Clone for ExecutionConfig<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypes> Copy for ExecutionConfig<T> {}

impl<T: EvmTypes> ExecutionConfig<T> {
    /// Creates an execution config for a base `SpecId` through selector `F`.
    ///
    /// The selector provides the concrete `Config<BASE_SPEC_ID>` used for the inherited base
    /// version.
    #[inline]
    pub const fn for_base_spec<F: EvmConfigSelector<T>>(base_spec_id: SpecId) -> Self
    where
        T: InstrTables<F>,
    {
        let i = base_spec_id as usize;
        let instructions = <T as InstrTables<F>>::INSTRUCTIONS;
        let inspect_instructions = <T as InstrTables<F>>::INSPECT_INSTRUCTIONS;
        Self {
            version: Version::new(base_spec_id),
            instructions: &instructions[i],
            inspect_instructions: &inspect_instructions[i],
        }
    }

    /// Creates an execution config for `spec_id` with dynamic runtime version data.
    #[inline]
    pub fn for_spec_and_version(spec_id: T::SpecId, version: Version) -> Self {
        let config = <T::ConfigSelector as EvmConfigSelector<T>>::execution_config(spec_id);
        assert_eq!(spec_id.into(), version.spec_id, "execution config version spec mismatch");
        config.with_version(version)
    }

    /// Replaces the runtime version data while keeping the same dispatch table.
    #[inline]
    pub fn with_version(mut self, version: Version) -> Self {
        assert_eq!(self.version.spec_id, version.spec_id, "execution config version spec mismatch");
        self.version = version;
        self
    }

    /// Returns the active EVM version.
    #[inline]
    pub const fn version(&self) -> &Version {
        &self.version
    }
}

/// Base EVM types.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmTypes(());

impl EvmTypes for BaseEvmTypes {
    type ConfigSelector = BaseEvmConfigSelector;
    type SpecId = SpecId;
    type Tx = RecoveredTxEnvelope;
    type Host = crate::evm::Evm<Self>;
}

/// Base EVM configuration for an inherited base specification ID.
///
/// `BASE_SPEC_ID` is the raw discriminant of `crate::SpecId`, not a custom runtime spec-id value.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfig<const BASE_SPEC_ID: u8>(());

impl<T: EvmTypes, const BASE_SPEC_ID: u8> EvmConfig<T> for BaseEvmConfig<BASE_SPEC_ID> {
    const BASE_SPEC_ID: SpecId = SpecId::try_from_u8(BASE_SPEC_ID).expect("invalid spec id");
    const VERSION_TABLES: &'static VersionTables<T> = &VersionTables::<T>::base::<Self>();
}

/// Base EVM config selector.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct BaseEvmConfigSelector(());

impl<T: EvmTypes> EvmConfigSelector<T> for BaseEvmConfigSelector {
    type Config<const BASE_SPEC_ID: u8> = BaseEvmConfig<BASE_SPEC_ID>;

    fn execution_config(spec_id: T::SpecId) -> ExecutionConfig<T> {
        ExecutionConfig::for_base_spec::<Self>(spec_id.into())
    }
}
