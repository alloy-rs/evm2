#![allow(dead_code)]

use crate::tx::{AccessListItem, TestAuthorization};
use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::SpecId;
use serde::{Deserialize, Deserializer, de};
use std::collections::BTreeMap;

/// Top-level state test suite.
#[derive(Debug, Deserialize)]
pub struct TestSuite(pub BTreeMap<String, TestUnit>);

/// A single named state test.
#[derive(Debug, Deserialize)]
pub struct TestUnit {
    /// Optional ethereum/tests metadata.
    #[serde(default, rename = "_info")]
    pub info: Option<serde_json::Value>,
    /// Block environment.
    pub env: Env,
    /// Pre-state accounts.
    pub pre: BTreeMap<Address, AccountInfo>,
    /// Expected post-state roots by fork.
    pub post: BTreeMap<SpecName, Vec<Test>>,
    /// Transaction parts indexed by each post entry.
    pub transaction: TransactionParts,
    /// Expected output.
    #[serde(default)]
    pub out: Option<Bytes>,
}

/// State test block environment.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Env {
    /// Chain ID for transaction execution.
    #[serde(rename = "currentChainID")]
    pub current_chain_id: Option<U256>,
    /// Block beneficiary.
    pub current_coinbase: Address,
    /// Pre-merge difficulty.
    #[serde(default)]
    pub current_difficulty: U256,
    /// Block gas limit.
    pub current_gas_limit: U256,
    /// Block number.
    pub current_number: U256,
    /// Block timestamp.
    pub current_timestamp: U256,
    /// EIP-1559 base fee.
    pub current_base_fee: Option<U256>,
    /// Previous block hash.
    pub previous_hash: Option<B256>,
    /// Post-merge randomness.
    pub current_random: Option<B256>,
    /// EIP-4788 beacon root.
    pub current_beacon_root: Option<B256>,
    /// Withdrawals root.
    pub current_withdrawals_root: Option<B256>,
    /// EIP-4844 excess blob gas.
    pub current_excess_blob_gas: Option<U256>,
    /// Beacon slot number.
    pub slot_number: Option<U256>,
}

/// State test account entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountInfo {
    /// Account balance.
    pub balance: U256,
    /// Account code.
    pub code: Bytes,
    /// Account nonce.
    #[serde(deserialize_with = "deserialize_str_as_u64")]
    pub nonce: u64,
    /// Account storage.
    pub storage: BTreeMap<U256, U256>,
}

/// State test transaction parts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionParts {
    /// Explicit transaction type.
    #[serde(rename = "type")]
    pub tx_type: Option<u8>,
    /// Input data variants.
    pub data: Vec<Bytes>,
    /// Gas limit variants.
    pub gas_limit: Vec<U256>,
    /// Legacy gas price.
    pub gas_price: Option<U256>,
    /// Transaction nonce.
    pub nonce: U256,
    /// Sender private key.
    #[serde(default)]
    pub secret_key: B256,
    /// Explicit sender.
    #[serde(default)]
    pub sender: Option<Address>,
    /// Transaction recipient, or none for create.
    #[serde(default, deserialize_with = "deserialize_maybe_empty")]
    pub to: Option<Address>,
    /// Value variants.
    #[serde(deserialize_with = "deserialize_u256_vec_allow_bigint")]
    pub value: Vec<U256>,
    /// EIP-1559 max fee.
    pub max_fee_per_gas: Option<U256>,
    /// EIP-1559 priority fee.
    pub max_priority_fee_per_gas: Option<U256>,
    /// EIP-7873 initcodes.
    pub initcodes: Option<Vec<Bytes>>,
    /// EIP-2930 access list variants.
    #[serde(default)]
    pub access_lists: Vec<Option<Vec<AccessListItem>>>,
    /// EIP-7702 authorizations.
    pub authorization_list: Option<Vec<TestAuthorization>>,
    /// EIP-4844 blob hashes.
    #[serde(default)]
    pub blob_versioned_hashes: Vec<B256>,
    /// EIP-4844 max fee per blob gas.
    pub max_fee_per_blob_gas: Option<U256>,
}

/// State test fork name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Hash)]
pub enum SpecName {
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
    /// EIP-150 aka Tangerine Whistle.
    #[serde(alias = "TangerineWhistle")]
    EIP150,
    /// EIP-158 aka Spurious Dragon.
    #[serde(alias = "SpuriousDragon")]
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
    pub const fn to_spec_id(self) -> Option<SpecId> {
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
            // Skip Constantinople due to the pre-Petersburg reentrancy bug.
            Self::Constantinople | Self::Unknown => None,
        }
    }
}

/// State test post entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Test {
    /// Expected exception.
    pub expect_exception: Option<String>,
    /// Transaction part indexes.
    pub indexes: TxPartIndices,
    /// Expected post-state root.
    pub hash: B256,
    /// Expected post-state account map.
    #[serde(default)]
    pub post_state: BTreeMap<Address, AccountInfo>,
    /// Expected logs root.
    pub logs: B256,
    /// Optional full expected state.
    #[serde(default)]
    pub state: BTreeMap<Address, AccountInfo>,
    /// Optional encoded transaction bytes.
    pub txbytes: Option<Bytes>,
}

/// Transaction part indexes.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TxPartIndices {
    /// Data index.
    pub data: usize,
    /// Gas index.
    pub gas: usize,
    /// Value index.
    pub value: usize,
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

fn deserialize_u256_vec_allow_bigint<'de, D>(deserializer: D) -> Result<Vec<U256>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| match &value {
            // retesteth uses 0x:bigint markers for intentionally invalid values.
            // evmone maps them to uint256::MAX so the invalid-transaction path can run.
            serde_json::Value::String(string) if string.starts_with("0x:bigint ") => Ok(U256::MAX),
            _ => serde_json::from_value(value).map_err(de::Error::custom),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_value_allows_bigint_marker() {
        let input = r#"{
            "data": ["0x"],
            "gasLimit": ["0x5208"],
            "gasPrice": "0x64",
            "nonce": "0x00",
            "to": "0xd0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0",
            "value": [
                "0x:bigint 0x10000000000000000000000000000000000000000000000000000000000000001"
            ]
        }"#;

        let tx: TransactionParts = serde_json::from_str(input).unwrap();

        assert_eq!(tx.value, vec![U256::MAX]);
    }
}
