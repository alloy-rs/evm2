#![allow(dead_code)]

use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::SpecId;
use serde::{Deserialize, Deserializer, de};
use std::collections::BTreeMap;

/// Top-level state test suite.
#[derive(Debug, Deserialize)]
pub(crate) struct TestSuite(pub(crate) BTreeMap<String, TestUnit>);

/// A single named state test.
#[derive(Debug, Deserialize)]
pub(crate) struct TestUnit {
    /// Optional ethereum/tests metadata.
    #[serde(default, rename = "_info")]
    pub(crate) info: Option<serde_json::Value>,
    /// Block environment.
    pub(crate) env: Env,
    /// Pre-state accounts.
    pub(crate) pre: BTreeMap<Address, AccountInfo>,
    /// Expected post-state roots by fork.
    pub(crate) post: BTreeMap<SpecName, Vec<Test>>,
    /// Transaction parts indexed by each post entry.
    pub(crate) transaction: TransactionParts,
    /// Expected output.
    #[serde(default)]
    pub(crate) out: Option<Bytes>,
}

/// State test block environment.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Env {
    /// Chain ID for transaction execution.
    #[serde(rename = "currentChainID")]
    pub(crate) current_chain_id: Option<U256>,
    /// Block beneficiary.
    pub(crate) current_coinbase: Address,
    /// Pre-merge difficulty.
    #[serde(default)]
    pub(crate) current_difficulty: U256,
    /// Block gas limit.
    pub(crate) current_gas_limit: U256,
    /// Block number.
    pub(crate) current_number: U256,
    /// Block timestamp.
    pub(crate) current_timestamp: U256,
    /// EIP-1559 base fee.
    pub(crate) current_base_fee: Option<U256>,
    /// Previous block hash.
    pub(crate) previous_hash: Option<B256>,
    /// Post-merge randomness.
    pub(crate) current_random: Option<B256>,
    /// EIP-4788 beacon root.
    pub(crate) current_beacon_root: Option<B256>,
    /// Withdrawals root.
    pub(crate) current_withdrawals_root: Option<B256>,
    /// EIP-4844 excess blob gas.
    pub(crate) current_excess_blob_gas: Option<U256>,
    /// Beacon slot number.
    pub(crate) slot_number: Option<U256>,
}

/// State test account entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AccountInfo {
    /// Account balance.
    pub(crate) balance: U256,
    /// Account code.
    pub(crate) code: Bytes,
    /// Account nonce.
    #[serde(deserialize_with = "deserialize_str_as_u64")]
    pub(crate) nonce: u64,
    /// Account storage.
    pub(crate) storage: BTreeMap<U256, U256>,
}

/// State test transaction parts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransactionParts {
    /// Explicit transaction type.
    #[serde(rename = "type")]
    pub(crate) tx_type: Option<u8>,
    /// Input data variants.
    pub(crate) data: Vec<Bytes>,
    /// Gas limit variants.
    pub(crate) gas_limit: Vec<U256>,
    /// Legacy gas price.
    pub(crate) gas_price: Option<U256>,
    /// Transaction nonce.
    pub(crate) nonce: U256,
    /// Sender private key.
    #[serde(default)]
    pub(crate) secret_key: B256,
    /// Explicit sender.
    #[serde(default)]
    pub(crate) sender: Option<Address>,
    /// Transaction recipient, or none for create.
    #[serde(default, deserialize_with = "deserialize_maybe_empty")]
    pub(crate) to: Option<Address>,
    /// Value variants.
    pub(crate) value: Vec<U256>,
    /// EIP-1559 max fee.
    pub(crate) max_fee_per_gas: Option<U256>,
    /// EIP-1559 priority fee.
    pub(crate) max_priority_fee_per_gas: Option<U256>,
    /// EIP-7873 initcodes.
    pub(crate) initcodes: Option<Vec<Bytes>>,
    /// EIP-2930 access list variants.
    #[serde(default)]
    pub(crate) access_lists: Vec<Option<Vec<AccessListItem>>>,
    /// EIP-7702 authorizations.
    pub(crate) authorization_list: Option<Vec<TestAuthorization>>,
    /// EIP-4844 blob hashes.
    #[serde(default)]
    pub(crate) blob_versioned_hashes: Vec<B256>,
    /// EIP-4844 max fee per blob gas.
    pub(crate) max_fee_per_blob_gas: Option<U256>,
}

/// Access list entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccessListItem {
    /// Accessed account.
    pub(crate) address: Address,
    /// Accessed storage keys.
    pub(crate) storage_keys: Vec<B256>,
}

/// EIP-7702 authorization entry.
#[derive(Clone, Debug)]
pub(crate) struct TestAuthorization {
    /// Raw authorization JSON.
    pub(crate) value: serde_json::Value,
}

impl<'de> Deserialize<'de> for TestAuthorization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let Some(object) = value.as_object_mut()
            && object.contains_key("v")
            && object.contains_key("yParity")
        {
            object.remove("v");
        }
        Ok(Self { value })
    }
}

/// State test fork name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Hash)]
pub(crate) enum SpecName {
    /// Frontier.
    Frontier,
    /// Frontier to Homestead transition.
    FrontierToHomesteadAt5,
    /// Homestead.
    Homestead,
    /// Homestead to DAO transition.
    HomesteadToDaoAt5,
    /// Homestead to EIP-150 transition.
    HomesteadToEIP150At5,
    /// EIP-150.
    EIP150,
    /// EIP-158.
    EIP158,
    /// EIP-158 to Byzantium transition.
    EIP158ToByzantiumAt5,
    /// Byzantium.
    Byzantium,
    /// Skipped Constantinople transition.
    ByzantiumToConstantinopleAt5,
    /// Byzantium to Petersburg transition.
    ByzantiumToConstantinopleFixAt5,
    /// Skipped Constantinople.
    Constantinople,
    /// Petersburg.
    ConstantinopleFix,
    /// Istanbul.
    Istanbul,
    /// Berlin.
    Berlin,
    /// Berlin to London transition.
    BerlinToLondonAt5,
    /// London.
    London,
    /// Paris.
    Paris,
    /// Merge.
    Merge,
    /// Shanghai.
    Shanghai,
    /// Cancun.
    Cancun,
    /// Prague.
    Prague,
    /// Osaka.
    Osaka,
    /// Amsterdam.
    Amsterdam,
    /// Unknown fork.
    #[serde(other)]
    Unknown,
}

impl SpecName {
    /// Converts the state test fork name to the local spec ID.
    #[inline]
    pub(crate) const fn to_spec_id(self) -> Option<SpecId> {
        match self {
            Self::Frontier => Some(SpecId::FRONTIER),
            Self::FrontierToHomesteadAt5 | Self::Homestead => Some(SpecId::HOMESTEAD),
            Self::HomesteadToDaoAt5 | Self::HomesteadToEIP150At5 | Self::EIP150 => {
                Some(SpecId::TANGERINE)
            }
            Self::EIP158 => Some(SpecId::SPURIOUS_DRAGON),
            Self::EIP158ToByzantiumAt5 | Self::Byzantium => Some(SpecId::BYZANTIUM),
            Self::ByzantiumToConstantinopleAt5
            | Self::ByzantiumToConstantinopleFixAt5
            | Self::Constantinople
            | Self::ConstantinopleFix => Some(SpecId::PETERSBURG),
            Self::Istanbul => Some(SpecId::ISTANBUL),
            Self::Berlin => Some(SpecId::BERLIN),
            Self::BerlinToLondonAt5 | Self::London => Some(SpecId::LONDON),
            Self::Paris | Self::Merge => Some(SpecId::MERGE),
            Self::Shanghai => Some(SpecId::SHANGHAI),
            Self::Cancun => Some(SpecId::CANCUN),
            Self::Prague => Some(SpecId::PRAGUE),
            Self::Osaka => Some(SpecId::OSAKA),
            Self::Amsterdam => Some(SpecId::AMSTERDAM),
            Self::Unknown => None,
        }
    }
}

/// State test post entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Test {
    /// Expected exception.
    pub(crate) expect_exception: Option<String>,
    /// Transaction part indexes.
    pub(crate) indexes: TxPartIndices,
    /// Expected post-state root.
    pub(crate) hash: B256,
    /// Expected post-state account map.
    #[serde(default)]
    pub(crate) post_state: BTreeMap<Address, AccountInfo>,
    /// Expected logs root.
    pub(crate) logs: B256,
    /// Optional full expected state.
    #[serde(default)]
    pub(crate) state: BTreeMap<Address, AccountInfo>,
    /// Optional encoded transaction bytes.
    pub(crate) txbytes: Option<Bytes>,
}

/// Transaction part indexes.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TxPartIndices {
    /// Data index.
    pub(crate) data: usize,
    /// Gas index.
    pub(crate) gas: usize,
    /// Value index.
    pub(crate) value: usize,
}

fn deserialize_str_as_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let string = String::deserialize(deserializer)?;
    if let Some(stripped) = string.strip_prefix("0x") {
        u64::from_str_radix(stripped, 16)
    } else {
        string.parse()
    }
    .map_err(de::Error::custom)
}

fn deserialize_maybe_empty<'de, D>(deserializer: D) -> Result<Option<Address>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(string) if string.is_empty() || string == "0x" => Ok(None),
        serde_json::Value::String(string) => string.parse().map(Some).map_err(de::Error::custom),
        _ => Err(de::Error::custom("invalid transaction to field")),
    }
}
