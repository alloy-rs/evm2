#![allow(dead_code)]

use crate::tx::{AccessListItem, TestAuthorization};
use alloy_eip7928::BlockAccessList;
use alloy_primitives::{Address, B256, Bytes, FixedBytes, TxKind, U256};
use serde::{Deserialize, Deserializer, de};
use std::collections::BTreeMap;

/// Top-level blockchain test suite.
#[derive(Debug, Deserialize)]
pub(crate) struct BlockchainTest(pub(crate) BTreeMap<String, BlockchainTestCase>);

/// Individual blockchain test case.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlockchainTestCase {
    /// Genesis block header.
    pub(crate) genesis_block_header: BlockHeader,
    /// Genesis block RLP encoding.
    #[serde(rename = "genesisRLP")]
    pub(crate) genesis_rlp: Option<Bytes>,
    /// Blocks in the test.
    pub(crate) blocks: Vec<Block>,
    /// Expected post-state accounts.
    pub(crate) post_state: Option<BTreeMap<Address, Account>>,
    /// Pre-state accounts.
    pub(crate) pre: State,
    /// Last block hash.
    pub(crate) lastblockhash: B256,
    /// Network specification.
    pub(crate) network: ForkSpec,
    /// Seal engine type.
    #[serde(default)]
    pub(crate) seal_engine: SealEngine,
}

/// Block header structure.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlockHeader {
    /// Bloom filter for logs.
    pub(crate) bloom: Bytes,
    /// Block coinbase/beneficiary address.
    pub(crate) coinbase: Address,
    /// Block difficulty.
    #[serde(default)]
    pub(crate) difficulty: U256,
    /// Extra data field.
    pub(crate) extra_data: Bytes,
    /// Block gas limit.
    pub(crate) gas_limit: U256,
    /// Block gas used.
    pub(crate) gas_used: U256,
    /// Block hash.
    pub(crate) hash: B256,
    /// Mix hash.
    #[serde(default)]
    pub(crate) mix_hash: B256,
    /// PoW nonce.
    #[serde(default)]
    pub(crate) nonce: FixedBytes<8>,
    /// Block number.
    pub(crate) number: U256,
    /// Parent block hash.
    pub(crate) parent_hash: B256,
    /// Receipt trie root.
    pub(crate) receipt_trie: B256,
    /// State root hash.
    pub(crate) state_root: B256,
    /// Block timestamp.
    pub(crate) timestamp: U256,
    /// Transaction trie root.
    pub(crate) transactions_trie: B256,
    /// Uncle hash.
    pub(crate) uncle_hash: B256,
    /// Base fee per gas.
    pub(crate) base_fee_per_gas: Option<U256>,
    /// Withdrawals root.
    pub(crate) withdrawals_root: Option<B256>,
    /// Blob gas used.
    pub(crate) blob_gas_used: Option<U256>,
    /// Excess blob gas.
    pub(crate) excess_blob_gas: Option<U256>,
    /// Parent beacon block root.
    pub(crate) parent_beacon_block_root: Option<B256>,
    /// Requests hash.
    pub(crate) requests_hash: Option<B256>,
    /// Target blobs per block.
    pub(crate) target_blobs_per_block: Option<U256>,
    /// Slot number.
    pub(crate) slot_number: Option<U256>,
}

/// Block structure containing header and transactions.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Block {
    /// Block header.
    pub(crate) block_header: Option<BlockHeader>,
    /// RLP-encoded block data.
    #[serde(default)]
    pub(crate) rlp: Bytes,
    /// Expected exception for invalid blocks.
    pub(crate) expect_exception: Option<String>,
    /// Transactions in the block.
    pub(crate) transactions: Option<Vec<Transaction>>,
    /// Uncle/ommer headers.
    pub(crate) uncle_headers: Option<Vec<BlockHeader>>,
    /// Withdrawals in the block.
    pub(crate) withdrawals: Option<Vec<Withdrawal>>,
    /// Block access list.
    pub(crate) block_access_list: Option<BlockAccessList>,
}

/// Transaction structure.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Transaction {
    /// Transaction type.
    #[serde(rename = "type")]
    pub(crate) transaction_type: Option<U256>,
    /// Transaction sender.
    #[serde(default)]
    pub(crate) sender: Option<Address>,
    /// Transaction data/input.
    pub(crate) data: Bytes,
    /// Gas limit.
    pub(crate) gas_limit: U256,
    /// Legacy gas price.
    pub(crate) gas_price: Option<U256>,
    /// Transaction nonce.
    pub(crate) nonce: U256,
    /// Signature r.
    pub(crate) r: U256,
    /// Signature s.
    pub(crate) s: U256,
    /// Signature v.
    pub(crate) v: U256,
    /// Ether value.
    pub(crate) value: U256,
    /// Target address.
    #[serde(default, deserialize_with = "deserialize_maybe_empty")]
    pub(crate) to: Option<Address>,
    /// Chain ID.
    pub(crate) chain_id: Option<U256>,
    /// Access list.
    #[serde(default)]
    pub(crate) access_list: Option<Vec<AccessListItem>>,
    /// Maximum fee per gas.
    pub(crate) max_fee_per_gas: Option<U256>,
    /// Maximum priority fee per gas.
    pub(crate) max_priority_fee_per_gas: Option<U256>,
    /// Blob versioned hashes.
    #[serde(default)]
    pub(crate) blob_versioned_hashes: Vec<B256>,
    /// Maximum fee per blob gas.
    pub(crate) max_fee_per_blob_gas: Option<U256>,
    /// Authorization list.
    pub(crate) authorization_list: Option<Vec<TestAuthorization>>,
    /// Transaction hash.
    pub(crate) hash: Option<B256>,
}

/// Withdrawal structure.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Withdrawal {
    /// Withdrawal index.
    pub(crate) index: U256,
    /// Validator index.
    pub(crate) validator_index: U256,
    /// Recipient address.
    pub(crate) address: Address,
    /// Amount in gwei.
    pub(crate) amount: U256,
}

/// Ethereum blockchain test data state.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct State(pub(crate) BTreeMap<Address, Account>);

/// An account.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Account {
    /// Balance.
    pub(crate) balance: U256,
    /// Code.
    pub(crate) code: Bytes,
    /// Nonce.
    pub(crate) nonce: U256,
    /// Storage.
    #[serde(default)]
    pub(crate) storage: BTreeMap<U256, U256>,
}

/// Fork specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub(crate) enum ForkSpec {
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
    /// Byzantium to Constantinople transition.
    ByzantiumToConstantinopleAt5,
    /// Byzantium to Petersburg transition.
    ByzantiumToConstantinopleFixAt5,
    /// Constantinople.
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
    /// Paris aka Merge.
    #[serde(alias = "Merge")]
    Paris,
    /// Paris to Shanghai transition.
    ParisToShanghaiAtTime15k,
    /// Shanghai.
    Shanghai,
    /// Shanghai to Cancun transition.
    ShanghaiToCancunAtTime15k,
    /// Merge EOF test.
    #[serde(alias = "Merge+3540+3670")]
    MergeEOF,
    /// Merge init code metering.
    #[serde(alias = "Merge+3860")]
    MergeMeterInitCode,
    /// Merge plus PUSH0.
    #[serde(alias = "Merge+3855")]
    MergePush0,
    /// Cancun.
    Cancun,
    /// Cancun to Prague transition.
    CancunToPragueAtTime15k,
    /// Prague.
    Prague,
    /// Prague to Osaka transition.
    PragueToOsakaAtTime15k,
    /// Osaka.
    Osaka,
    /// BPO1 to BPO2 transition.
    BPO1ToBPO2AtTime15k,
    /// BPO2 to Amsterdam transition.
    BPO2ToAmsterdamAtTime15k,
    /// Amsterdam.
    Amsterdam,
}

impl ForkSpec {
    /// Returns true if this fork name represents a transition fork.
    pub(crate) const fn is_transition(self) -> bool {
        matches!(
            self,
            Self::FrontierToHomesteadAt5
                | Self::HomesteadToDaoAt5
                | Self::HomesteadToEIP150At5
                | Self::EIP158ToByzantiumAt5
                | Self::ByzantiumToConstantinopleAt5
                | Self::ByzantiumToConstantinopleFixAt5
                | Self::BerlinToLondonAt5
                | Self::ParisToShanghaiAtTime15k
                | Self::ShanghaiToCancunAtTime15k
                | Self::CancunToPragueAtTime15k
                | Self::PragueToOsakaAtTime15k
                | Self::BPO1ToBPO2AtTime15k
                | Self::BPO2ToAmsterdamAtTime15k
        )
    }
}

/// Possible seal engines.
#[derive(Debug, Default, Deserialize)]
pub(crate) enum SealEngine {
    /// No consensus checks.
    #[default]
    NoProof,
    /// Proof of work.
    Ethash,
}

/// Converts an optional address field that may be encoded as an empty string.
fn deserialize_maybe_empty<'de, D>(deserializer: D) -> Result<Option<Address>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(string) if string.is_empty() || string == "0x" => Ok(None),
        serde_json::Value::String(string) => string.parse().map(Some).map_err(de::Error::custom),
        other => serde_json::from_value(other).map(Some).map_err(de::Error::custom),
    }
}

impl Transaction {
    /// Returns this transaction's EIP-2718 type byte.
    pub(crate) fn tx_type(&self) -> u8 {
        self.transaction_type.map(|ty| ty.to::<u8>()).unwrap_or(0)
    }

    /// Returns this transaction's target kind.
    pub(crate) fn kind(&self) -> TxKind {
        self.to.map_or(TxKind::Create, TxKind::Call)
    }
}
