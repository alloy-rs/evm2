#![allow(dead_code)]

use crate::tx::{AccessListItem, TestAuthorization};
use alloy_eip7928::BlockAccessList;
use alloy_primitives::{Address, B256, Bytes, FixedBytes, TxKind, U256};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeMap;

/// Top-level blockchain test suite.
#[derive(Debug, Deserialize, Serialize)]
pub struct BlockchainTest(
    /// Test cases keyed by name.
    pub BTreeMap<String, BlockchainTestCase>,
);

/// Individual blockchain test case.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockchainTestCase {
    /// Genesis block header.
    pub genesis_block_header: BlockHeader,
    /// Genesis block RLP encoding.
    #[serde(rename = "genesisRLP", skip_serializing_if = "Option::is_none")]
    pub genesis_rlp: Option<Bytes>,
    /// Blocks in the test.
    pub blocks: Vec<Block>,
    /// Expected post-state accounts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_state: Option<BTreeMap<Address, Account>>,
    /// Pre-state accounts.
    pub pre: State,
    /// Historical block hashes available to the `BLOCKHASH` opcode before the first block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_hashes: Vec<BlockHash>,
    /// Last block hash.
    pub lastblockhash: B256,
    /// Network specification.
    pub network: ForkSpec,
    /// Seal engine type.
    #[serde(default)]
    pub seal_engine: SealEngine,
}

/// Historical block hash entry.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockHash {
    /// Block number.
    pub number: U256,
    /// Block hash.
    pub hash: B256,
}

/// Block header structure.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockHeader {
    /// Bloom filter for logs.
    pub bloom: Bytes,
    /// Block coinbase/beneficiary address.
    pub coinbase: Address,
    /// Block difficulty.
    #[serde(default)]
    pub difficulty: U256,
    /// Extra data field.
    pub extra_data: Bytes,
    /// Block gas limit.
    pub gas_limit: U256,
    /// Block gas used.
    pub gas_used: U256,
    /// Block hash.
    pub hash: B256,
    /// Mix hash.
    #[serde(default)]
    pub mix_hash: B256,
    /// PoW nonce.
    #[serde(default)]
    pub nonce: FixedBytes<8>,
    /// Block number.
    pub number: U256,
    /// Parent block hash.
    pub parent_hash: B256,
    /// Receipt trie root.
    pub receipt_trie: B256,
    /// State root hash.
    pub state_root: B256,
    /// Block timestamp.
    pub timestamp: U256,
    /// Transaction trie root.
    pub transactions_trie: B256,
    /// Uncle hash.
    pub uncle_hash: B256,
    /// Base fee per gas.
    pub base_fee_per_gas: Option<U256>,
    /// Withdrawals root.
    pub withdrawals_root: Option<B256>,
    /// Blob gas used.
    pub blob_gas_used: Option<U256>,
    /// Excess blob gas.
    pub excess_blob_gas: Option<U256>,
    /// Parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// Requests hash.
    pub requests_hash: Option<B256>,
    /// Target blobs per block.
    pub target_blobs_per_block: Option<U256>,
    /// Slot number.
    pub slot_number: Option<U256>,
}

/// Block structure containing header and transactions.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    /// Block header.
    pub block_header: Option<BlockHeader>,
    /// Decoded block payload used by blockchain fixtures that primarily carry block RLP.
    #[serde(rename = "rlp_decoded")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rlp_decoded: Option<DecodedBlock>,
    /// RLP-encoded block data.
    #[serde(default, skip_serializing_if = "is_empty_bytes")]
    pub rlp: Bytes,
    /// Expected exception for invalid blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_exception: Option<String>,
    /// Transactions in the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transactions: Option<Vec<Transaction>>,
    /// Uncle/ommer headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncle_headers: Option<Vec<BlockHeader>>,
    /// Withdrawals in the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdrawals: Option<Vec<Withdrawal>>,
    /// Block access list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_access_list: Option<BlockAccessList>,
}

/// Decoded contents of an RLP-backed block fixture.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedBlock {
    /// Block header.
    pub block_header: Option<BlockHeader>,
    /// Transactions in the block.
    #[serde(default)]
    pub transactions: Vec<Transaction>,
    /// Uncle/ommer headers.
    #[serde(default)]
    pub uncle_headers: Vec<BlockHeader>,
    /// Withdrawals in the block.
    #[serde(default)]
    pub withdrawals: Vec<Withdrawal>,
}

/// Transaction structure.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    /// Transaction type.
    #[serde(rename = "type")]
    pub transaction_type: Option<U256>,
    /// Transaction sender.
    #[serde(default)]
    pub sender: Option<Address>,
    /// Transaction data/input.
    pub data: Bytes,
    /// Gas limit.
    pub gas_limit: U256,
    /// Legacy gas price.
    pub gas_price: Option<U256>,
    /// Transaction nonce.
    pub nonce: U256,
    /// Signature r.
    pub r: U256,
    /// Signature s.
    pub s: U256,
    /// Signature v.
    pub v: U256,
    /// Ether value.
    pub value: U256,
    /// Target address.
    #[serde(default, deserialize_with = "deserialize_maybe_empty")]
    pub to: Option<Address>,
    /// Chain ID.
    pub chain_id: Option<U256>,
    /// Access list.
    #[serde(default)]
    pub access_list: Option<Vec<AccessListItem>>,
    /// Maximum fee per gas.
    pub max_fee_per_gas: Option<U256>,
    /// Maximum priority fee per gas.
    pub max_priority_fee_per_gas: Option<U256>,
    /// Blob versioned hashes.
    #[serde(default)]
    pub blob_versioned_hashes: Vec<B256>,
    /// Maximum fee per blob gas.
    pub max_fee_per_blob_gas: Option<U256>,
    /// Authorization list.
    pub authorization_list: Option<Vec<TestAuthorization>>,
    /// Transaction hash.
    pub hash: Option<B256>,
}

/// Withdrawal structure.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Withdrawal {
    /// Withdrawal index.
    pub index: U256,
    /// Validator index.
    pub validator_index: U256,
    /// Recipient address.
    pub address: Address,
    /// Amount in gwei.
    pub amount: U256,
}

/// Ethereum blockchain test data state.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct State(
    /// Accounts keyed by address.
    pub BTreeMap<Address, Account>,
);

/// An account.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Account {
    /// Balance.
    pub balance: U256,
    /// Code.
    pub code: Bytes,
    /// Nonce.
    pub nonce: U256,
    /// Storage.
    #[serde(default)]
    pub storage: BTreeMap<U256, U256>,
}

/// Fork specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
pub enum ForkSpec {
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
    /// Osaka to BPO1 transition.
    OsakaToBPO1AtTime15k,
    /// BPO1 to BPO2 transition.
    BPO1ToBPO2AtTime15k,
    /// BPO2 to BPO3 transition.
    BPO2ToBPO3AtTime15k,
    /// BPO3 to BPO4 transition.
    BPO3ToBPO4AtTime15k,
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
                | Self::OsakaToBPO1AtTime15k
                | Self::BPO1ToBPO2AtTime15k
                | Self::BPO2ToBPO3AtTime15k
                | Self::BPO3ToBPO4AtTime15k
                | Self::BPO2ToAmsterdamAtTime15k
        )
    }
}

/// Possible seal engines.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub enum SealEngine {
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
    if !deserializer.is_human_readable() {
        return Option::<Address>::deserialize(deserializer);
    }

    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(string) if string.is_empty() || string == "0x" => Ok(None),
        serde_json::Value::String(string) => string.parse().map(Some).map_err(de::Error::custom),
        other => serde_json::from_value(other).map(Some).map_err(de::Error::custom),
    }
}

fn is_empty_bytes(bytes: &Bytes) -> bool {
    bytes.as_ref().is_empty()
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
