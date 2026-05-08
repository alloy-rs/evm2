//! Custom EVM type family and version selector.

use crate::opcode;
use evm2::{
    BaseEvmConfig, Evm, EvmConfig, EvmConfigSelector, EvmTypes, ExecutionConfig, SpecId, Version,
    VersionTables,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CustomSpecId {
    MainnetOsaka,
    CustomOsaka,
}

impl From<CustomSpecId> for SpecId {
    fn from(spec_id: CustomSpecId) -> Self {
        match spec_id {
            CustomSpecId::MainnetOsaka | CustomSpecId::CustomOsaka => Self::OSAKA,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CustomTypes;

impl EvmTypes for CustomTypes {
    type ConfigSelector = CustomConfigSelector;
    type SpecId = CustomSpecId;
    type Tx = crate::tx::CustomEnvelope;
    type Host = Evm<Self>;
}

// Const-generic configs are still keyed by the inherited base spec.
pub(crate) struct CustomConfig<const BASE_SPEC_ID: u8>(());

impl<const ID: u8> EvmConfig<CustomTypes> for CustomConfig<ID> {
    const BASE_SPEC_ID: SpecId = SpecId::try_from_u8(ID).unwrap();
    const VERSION_TABLES: &'static VersionTables<CustomTypes> = &custom_version_tables::<ID>();
}

pub(crate) const fn custom_version(base_spec_id: SpecId) -> Version {
    let mut version = *Version::base(base_spec_id);
    opcode::install_gas_params(&mut version.gas_params);
    version
}

const fn custom_version_tables<const BASE_SPEC_ID: u8>() -> VersionTables<CustomTypes> {
    let mut version = VersionTables::<CustomTypes>::base::<CustomConfig<BASE_SPEC_ID>>();
    version.set_instruction::<opcode::custom<CustomTypes>>(
        opcode::CUSTOM_OPCODE,
        opcode::CUSTOM_OPCODE_GAS,
    );
    version
}

pub(crate) struct CustomConfigSelector(());

impl EvmConfigSelector<CustomTypes> for CustomConfigSelector {
    type Config<const BASE_SPEC_ID: u8> = CustomConfig<BASE_SPEC_ID>;

    fn execution_config(spec_id: CustomSpecId) -> ExecutionConfig<CustomTypes> {
        match spec_id {
            // Use unmodified revm-compatible Osaka tables.
            CustomSpecId::MainnetOsaka => {
                ExecutionConfig::for_config::<BaseEvmConfig<{ SpecId::OSAKA as u8 }>>()
            }
            // Use the same base spec, with the custom tables from `CustomConfig`.
            CustomSpecId::CustomOsaka => {
                let base_spec_id = spec_id.into();
                ExecutionConfig::for_base_spec::<Self>(base_spec_id)
            }
        }
    }
}
