//! Custom EVM type family and version selector.

use crate::opcode;
use evm2::{
    BaseEvmConfig, Evm, EvmConfig, EvmConfigSelector, EvmTypes, ExecutionConfig, SpecId, Version,
    VersionTables,
};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomSpecId {
    MainnetOsaka,
    CustomOsaka,
}

impl CustomSpecId {
    pub const MIN: Self = Self::MainnetOsaka;
    pub const NEXT: Self = Self::CustomOsaka;
    pub const COUNT: usize = Self::NEXT as usize - Self::MIN as usize + 1;

    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn try_from_u8(spec_id: u8) -> Option<Self> {
        if spec_id <= Self::NEXT as u8 {
            // SAFETY: `spec_id` is within the valid variant range.
            return Some(unsafe { core::mem::transmute::<u8, Self>(spec_id) });
        }
        None
    }

    pub const fn enables(self, other: Self) -> bool {
        self as u8 >= other as u8
    }
}

impl From<CustomSpecId> for u8 {
    fn from(spec_id: CustomSpecId) -> Self {
        spec_id as Self
    }
}

impl From<CustomSpecId> for SpecId {
    fn from(spec_id: CustomSpecId) -> Self {
        match spec_id {
            CustomSpecId::MainnetOsaka | CustomSpecId::CustomOsaka => Self::OSAKA,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CustomTypes;

impl EvmTypes for CustomTypes {
    type ConfigSelector = CustomConfigSelector;
    type SpecId = CustomSpecId;
    type Tx = crate::tx::CustomEnvelope;
    type MessageExt = CustomMessageExt;
    type MessageResultExt = CustomMessageResultExt;
    type TxEnvExt = CustomTxEnvExt;
    type TxResultExt = CustomTxResultExt;
    type BlockEnvExt = CustomBlockEnvExt;
    type Host = Evm<Self>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CustomMessageExt {
    pub is_system: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CustomMessageResultExt {
    pub handled_custom_message: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CustomTxEnvExt {
    pub label: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CustomTxResultExt {
    pub handled_custom_tx: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CustomBlockEnvExt {
    pub l1_block_number: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct CustomConfig<const BASE_SPEC_ID: u8, const CUSTOM_SPEC_ID: u8>(());

impl<const BASE_SPEC_ID: u8, const CUSTOM_SPEC_ID: u8> EvmConfig<CustomTypes>
    for CustomConfig<BASE_SPEC_ID, CUSTOM_SPEC_ID>
{
    const BASE_SPEC_ID: SpecId = SpecId::try_from_u8(BASE_SPEC_ID).unwrap();
    const VERSION_TABLES: &'static VersionTables<CustomTypes> =
        &custom_version_tables::<BASE_SPEC_ID, CUSTOM_SPEC_ID>();
}

pub const fn custom_version(base_spec_id: SpecId) -> Version {
    let mut version = *Version::base(base_spec_id);
    opcode::install_gas_params(&mut version.gas_params);
    version
}

pub const fn custom_version_tables<const BASE_SPEC_ID: u8, const CUSTOM_SPEC_ID: u8>()
-> VersionTables<CustomTypes> {
    let mut version =
        VersionTables::<CustomTypes>::base::<CustomConfig<BASE_SPEC_ID, CUSTOM_SPEC_ID>>();
    let custom_spec_id = CustomSpecId::try_from_u8(CUSTOM_SPEC_ID).expect("invalid custom spec id");
    if custom_spec_id.enables(CustomSpecId::CustomOsaka) {
        version.set_instruction::<opcode::custom<CustomTypes>>(
            opcode::CUSTOM_OPCODE,
            opcode::CUSTOM_OPCODE_GAS,
        );
        version.set_instruction::<opcode::l1_blocknumber>(
            opcode::L1_BLOCKNUMBER_OPCODE,
            opcode::L1_BLOCKNUMBER_GAS,
        );
    }
    version
}

#[derive(Clone, Copy, Debug)]
pub struct CustomConfigSelector(());

impl EvmConfigSelector<CustomTypes> for CustomConfigSelector {
    type Config<const BASE_SPEC_ID: u8, const CUSTOM_SPEC_ID: u8> =
        CustomConfig<BASE_SPEC_ID, CUSTOM_SPEC_ID>;

    fn execution_config(spec_id: CustomSpecId) -> ExecutionConfig<CustomTypes> {
        match spec_id {
            // Use unmodified Osaka tables.
            CustomSpecId::MainnetOsaka => {
                ExecutionConfig::for_config::<BaseEvmConfig<{ SpecId::OSAKA as u8 }>>()
            }
            // Use the same base spec, with one concrete custom table.
            CustomSpecId::CustomOsaka => ExecutionConfig::for_config::<
                CustomConfig<{ SpecId::OSAKA as u8 }, { CustomSpecId::CustomOsaka.as_u8() }>,
            >(),
        }
    }
}
