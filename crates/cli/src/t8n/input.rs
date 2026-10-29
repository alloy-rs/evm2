//! Parsing of the `alloc.json`, `env.json` and `txs.json` inputs shared with go-ethereum's `evm
//! t8n`.

use super::Error;
use alloy_consensus::{SignableTransaction, TxEnvelope};
use alloy_eips::eip4895::Withdrawal;
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256};
use alloy_rpc_types_eth::AccessList;
use evm2::{SpecId, ethereum::RecoveredTxEnvelope};
use evm2_eest::{
    TestAuthorization, TxFields, build_typed_tx, recover_address, recovered_envelope,
    signed_authorizations,
};
use secp256k1::{Message, SecretKey, ecdsa::RecoverableSignature};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

/// Chain ID used to sign protected transactions, as in `evm t8n`.
const CHAIN_ID: u64 = 1;

/// One account of the pre-state.
#[derive(Debug, Default, Deserialize)]
pub struct AllocAccount {
    /// Balance in wei.
    #[serde(default)]
    pub balance: U256,
    /// Account code.
    #[serde(default)]
    pub code: Bytes,
    /// Account nonce.
    #[serde(default)]
    pub nonce: U256,
    /// Account storage.
    #[serde(default)]
    pub storage: BTreeMap<U256, U256>,
}

/// Pre-state accounts by address.
pub type Alloc = BTreeMap<Address, AllocAccount>;

/// Block environment, in the field names of `evm t8n`'s `env.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Env {
    /// Block beneficiary.
    pub current_coinbase: Address,
    /// Pre-merge difficulty.
    pub current_difficulty: Option<U256>,
    /// Post-merge randomness.
    pub current_random: Option<U256>,
    /// Block gas limit.
    pub current_gas_limit: U256,
    /// Block number.
    pub current_number: U256,
    /// Block timestamp.
    pub current_timestamp: U256,
    /// EIP-1559 base fee.
    pub current_base_fee: Option<U256>,
    /// EIP-4844 excess blob gas.
    pub current_excess_blob_gas: Option<U256>,
    /// Beacon slot number.
    pub slot_number: Option<U256>,
    /// Parent base fee; only to detect a missing `currentBaseFee`.
    pub parent_base_fee: Option<U256>,
    /// Parent excess blob gas; only to detect a missing `currentExcessBlobGas`.
    pub parent_excess_blob_gas: Option<U256>,
    /// EIP-4788 parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// Block hashes by block number, for `BLOCKHASH` and EIP-2935.
    #[serde(default, deserialize_with = "block_hashes")]
    pub block_hashes: Option<HashMap<u64, B256>>,
    /// Withdrawals applied after the transactions.
    pub withdrawals: Option<Vec<Withdrawal>>,
}

/// A transaction in the JSON form that `evm t8n` signs with `secretKey`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxJson {
    #[serde(rename = "type")]
    tx_type: Option<U256>,
    #[serde(default)]
    nonce: U256,
    gas_price: Option<U256>,
    max_fee_per_gas: Option<U256>,
    max_priority_fee_per_gas: Option<U256>,
    #[serde(default)]
    gas: U256,
    to: Option<Address>,
    #[serde(default)]
    value: U256,
    #[serde(default, alias = "data")]
    input: Bytes,
    access_list: Option<AccessList>,
    chain_id: Option<U256>,
    #[serde(default)]
    blob_versioned_hashes: Vec<B256>,
    max_fee_per_blob_gas: Option<U256>,
    authorization_list: Option<Vec<TestAuthorization>>,
    secret_key: Option<B256>,
    protected: Option<bool>,
    #[serde(default)]
    v: U256,
    #[serde(default)]
    r: U256,
    #[serde(default)]
    s: U256,
}

/// A signed transaction ready for execution.
pub(super) struct SignedTx {
    /// Sender recovered from the secret key.
    pub(super) caller: Address,
    /// Signed consensus transaction, for its hash and encoding.
    pub(super) envelope: TxEnvelope,
    /// Transaction recipient, or create.
    pub(super) kind: TxKind,
    /// Sender nonce.
    pub(super) nonce: u64,
    /// Transaction in the form the EVM executes.
    pub(super) recovered: RecoveredTxEnvelope,
}

impl TxJson {
    /// Whether this is a blob or set-code transaction without `to`, which `evm t8n` cannot decode.
    pub(super) fn lacks_recipient(&self) -> bool {
        self.to.is_none() && matches!(self.tx_type.map(u8::try_from), Some(Ok(3 | 4)))
    }

    /// Signs the transaction with its `secretKey`, as `evm t8n` does.
    pub(super) fn sign(&self) -> Result<SignedTx, String> {
        if !(self.v.is_zero() && self.r.is_zero() && self.s.is_zero()) {
            return Err("pre-signed transactions are not supported, use `secretKey`".to_string());
        }
        let key = self.secret_key.ok_or("missing `secretKey`")?;
        let caller = recover_address(key.as_slice()).ok_or("invalid `secretKey`")?;
        let tx_type = match self.tx_type {
            Some(ty) => u8::try_from(ty).map_err(|_| "invalid transaction type")?,
            None => 0,
        };
        let chain_id = match tx_type {
            0 => self
                .chain_id
                .or_else(|| self.protected.unwrap_or(true).then_some(U256::from(CHAIN_ID))),
            _ => Some(self.chain_id.unwrap_or(U256::from(CHAIN_ID))),
        };
        let nonce = u64::try_from(self.nonce).map_err(|_| "nonce overflows u64")?;
        let kind = TxKind::from(self.to);
        let typed = build_typed_tx(TxFields {
            tx_type: Some(tx_type),
            caller,
            kind,
            data: self.input.clone(),
            gas_limit: u64::try_from(self.gas).map_err(|_| "gas overflows u64")?,
            nonce,
            value: self.value,
            chain_id,
            gas_price: self.gas_price,
            max_fee_per_gas: self.max_fee_per_gas,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas,
            access_list: (tx_type != 0).then(|| self.access_list.clone().unwrap_or_default()),
            authorization_list: signed_authorizations(self.authorization_list.as_deref())
                .map_err(|err| err.to_string())?,
            blob_versioned_hashes: self.blob_versioned_hashes.clone(),
            max_fee_per_blob_gas: self.max_fee_per_blob_gas,
        })
        .map_err(|err| err.to_string())?;

        let secret = SecretKey::from_secret_bytes(key.0).map_err(|err| err.to_string())?;
        let (recovery_id, compact) = RecoverableSignature::sign_ecdsa_recoverable(
            Message::from_digest(typed.signature_hash().0),
            &secret,
        )
        .serialize_compact();
        let signature = Signature::new(
            U256::from_be_slice(&compact[..32]),
            U256::from_be_slice(&compact[32..]),
            u8::from(recovery_id) != 0,
        );
        let envelope = TxEnvelope::new_unhashed(typed.clone(), signature);
        Ok(SignedTx { caller, envelope, kind, nonce, recovered: recovered_envelope(typed, caller) })
    }
}

/// Everything `evm2 t8n` reads before executing.
#[derive(Debug)]
pub struct Inputs {
    /// Pre-state.
    pub alloc: Alloc,
    /// Block environment.
    pub env: Env,
    /// Unsigned transactions.
    pub txs: Vec<TxJson>,
    /// Hardfork to execute under.
    pub spec: SpecId,
    /// Block reward in wei, or negative to leave the coinbase untouched.
    pub reward: i64,
}

impl Inputs {
    /// Reads the three input files and resolves the fork name.
    pub fn read(
        alloc: &Path,
        env: &Path,
        txs: &Path,
        fork: &str,
        reward: i64,
    ) -> Result<Self, Error> {
        Ok(Self {
            alloc: read_json(alloc)?,
            env: read_json(env)?,
            txs: read_json(txs)?,
            spec: parse_fork(fork)?,
            reward,
        })
    }
}

/// Reads block hashes keyed by decimal or `0x` hex block number, as `evm t8n` does.
fn block_hashes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<HashMap<u64, B256>>, D::Error> {
    let hashes = Option::<BTreeMap<String, B256>>::deserialize(deserializer)?;
    hashes
        .map(|hashes| {
            hashes
                .into_iter()
                .map(|(number, hash)| {
                    let parsed = match number.strip_prefix("0x") {
                        Some(hex) => u64::from_str_radix(hex, 16),
                        None => number.parse(),
                    };
                    parsed.map(|number| (number, hash)).map_err(|_| {
                        serde::de::Error::custom(format!("invalid block hash number `{number}`"))
                    })
                })
                .collect()
        })
        .transpose()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let data =
        std::fs::read(path).map_err(|source| Error::Read { path: path.to_path_buf(), source })?;
    serde_json::from_slice(&data).map_err(|source| Error::Json { path: path.to_path_buf(), source })
}

/// Resolves a go-ethereum fork name to the matching evm2 specification.
pub fn parse_fork(name: &str) -> Result<SpecId, Error> {
    Ok(match name {
        "Frontier" => SpecId::FRONTIER,
        "Homestead" => SpecId::HOMESTEAD,
        "EIP150" => SpecId::TANGERINE,
        "EIP158" => SpecId::SPURIOUS_DRAGON,
        "Byzantium" => SpecId::BYZANTIUM,
        "ConstantinopleFix" => SpecId::PETERSBURG,
        "Istanbul" | "MuirGlacier" => SpecId::ISTANBUL,
        "Berlin" => SpecId::BERLIN,
        "London" | "ArrowGlacier" | "GrayGlacier" => SpecId::LONDON,
        "Merge" | "Paris" => SpecId::MERGE,
        "Shanghai" => SpecId::SHANGHAI,
        "Cancun" => SpecId::CANCUN,
        "Prague" => SpecId::PRAGUE,
        "Osaka" => SpecId::OSAKA,
        "Amsterdam" => SpecId::AMSTERDAM,
        _ => return Err(Error::UnsupportedFork(name.to_string())),
    })
}
