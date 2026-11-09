use crate::{
    blockchaintest::{ChainConfig, ForkSpec},
    tx::{AccessListItem, TestAuthorization},
};
use alloy_primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Deserializer, de};
use std::{collections::BTreeMap, fmt};

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
    #[serde(deserialize_with = "deserialize_post")]
    pub post: BTreeMap<ForkSpec, Vec<Test>>,
    /// Transaction parts indexed by each post entry.
    pub transaction: TransactionParts,
    /// Expected output.
    #[serde(default)]
    pub out: Option<Bytes>,
    /// Chain configuration the test ran under.
    #[serde(default)]
    pub config: Option<ChainConfig>,
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
    /// Explicit transaction chain id. When present it overrides the environment chain id, so a
    /// fixture can model a transaction whose chain id differs from the network's (expected to be
    /// rejected with `INVALID_CHAINID`), including for type-0 transactions.
    #[serde(default)]
    pub chain_id: Option<U256>,
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
    /// Signed EIP-2718 transaction bytes. When present they replace the indexed `transaction`
    /// fields and the sender is recovered from their signature.
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

/// Deserializes the post entries, rejecting two keys that name the same fork, such as `Paris` and
/// its alias `Merge`.
fn deserialize_post<'de, D>(deserializer: D) -> Result<BTreeMap<ForkSpec, Vec<Test>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PostVisitor;

    impl<'de> de::Visitor<'de> for PostVisitor {
        type Value = BTreeMap<ForkSpec, Vec<Test>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map of fork names to post entries")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut post = BTreeMap::new();
            while let Some((fork, tests)) = map.next_entry::<ForkSpec, Vec<Test>>()? {
                if post.insert(fork, tests).is_some() {
                    return Err(de::Error::custom(format!("duplicate post entry for {fork:?}")));
                }
            }
            Ok(post)
        }
    }

    deserializer.deserialize_map(PostVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Post(#[serde(deserialize_with = "deserialize_post")] BTreeMap<ForkSpec, Vec<Test>>);

    #[test]
    fn post_rejects_two_keys_for_one_fork() {
        assert!(serde_json::from_str::<Post>(r#"{"Paris": [], "Merge": []}"#).is_err());
        assert!(serde_json::from_str::<Post>(r#"{"Merge": [], "Paris": []}"#).is_err());
        assert_eq!(
            serde_json::from_str::<Post>(r#"{"Paris": [], "Cancun": []}"#).unwrap().0.len(),
            2
        );
    }

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
