use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use evm2::{
    SpecId, Version,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::RecoveredTxEnvelope,
    evm::{AccountInfo, InMemoryDB},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
};

#[derive(Debug)]
pub(crate) struct Suites(HashMap<&'static str, Suite>);

impl Suites {
    pub(crate) fn load(paths: impl IntoIterator<Item = &'static str>) -> Self {
        let mut suites = HashMap::new();
        for path in paths {
            suites.entry(path).or_insert_with(|| {
                let path = workspace_path(path);
                let input = fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("failed to read {}: {e}", path.display());
                });
                Suite::parse(&input)
            });
        }
        Self(suites)
    }

    pub(crate) fn get(&self, path: &'static str) -> &Suite {
        self.0.get(path).expect("fixture suite must be loaded")
    }
}

fn workspace_path(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}

#[derive(Debug, Deserialize)]
pub(crate) struct Suite(BTreeMap<String, Case>);

impl Suite {
    pub(crate) fn parse(input: &str) -> Self {
        serde_json::from_str(input).expect("fixture must parse")
    }

    pub(crate) fn case(&self, name: &str) -> &Case {
        if let Some(case) = self.0.get(name) {
            return case;
        }
        if self.0.len() == 1 {
            return self.0.values().next().expect("fixture must contain a case");
        }
        panic!("fixture suite does not contain benchmark case {name}");
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Case {
    env: Env,
    pre: BTreeMap<Address, Account>,
    transaction: Vec<Transaction>,
}

impl Case {
    pub(crate) fn block(&self) -> BlockEnv {
        self.env.block()
    }

    pub(crate) fn state(&self) -> InMemoryDB {
        let mut db = InMemoryDB::default();
        for (address, account) in &self.pre {
            let mut info =
                AccountInfo::default().with_code(Bytecode::new_legacy(account.code.clone()));
            info.balance = account.balance;
            info.nonce = u64_value(account.nonce);
            db.insert_account_info(*address, info);

            for (key, value) in &account.storage {
                db.insert_account_storage(*address, *key, *value);
            }
        }
        db
    }

    pub(crate) fn tx(&self, spec: SpecId) -> RecoveredTxEnvelope {
        self.transaction.first().expect("fixture must contain a transaction").envelope(spec)
    }

    pub(crate) fn revm_case(&self, name: &str, spec: SpecId) -> revm_fixture::Case {
        let transaction = self.transaction.first().expect("fixture must contain a transaction");
        let gas_limit = transaction.capped_gas_limit(spec);
        let value = transaction.value.unwrap_or_default();
        let current_random = self.env.current_random.map(b256_from_u256);
        let fixture = json!({
            name: {
                "env": {
                    "currentChainID": null,
                    "currentCoinbase": self.env.current_coinbase,
                    "currentDifficulty": self.env.current_difficulty,
                    "currentGasLimit": self.env.current_gas_limit,
                    "currentNumber": self.env.current_number,
                    "currentTimestamp": self.env.current_timestamp,
                    "currentBaseFee": self.env.current_base_fee,
                    "previousHash": null,
                    "currentRandom": current_random,
                    "currentBeaconRoot": null,
                    "currentWithdrawalsRoot": null,
                    "currentExcessBlobGas": null,
                    "slotNumber": self.env.slot_number,
                },
                "pre": self.pre,
                "post": {
                    revm_spec_name(spec): [{
                        "expectException": null,
                        "indexes": {
                            "data": 0,
                            "gas": 0,
                            "value": 0,
                        },
                        "hash": B256::ZERO,
                        "logs": B256::ZERO,
                    }],
                },
                "transaction": {
                    "data": [transaction.data.clone()],
                    "gasLimit": [U256::from(gas_limit)],
                    "gasPrice": transaction.gas_price,
                    "nonce": transaction.nonce,
                    "secretKey": B256::ZERO,
                    "sender": transaction.sender,
                    "to": transaction.to,
                    "value": [value],
                },
            },
        });
        let mut suite: ::revm::statetest_types::TestSuite = serde_json::from_value(fixture)
            .expect("converted fixture must parse as revm statetest");
        let mut unit = suite.0.remove(name).expect("converted suite must contain benchmark case");
        let test = unit
            .post
            .values_mut()
            .next()
            .and_then(|tests| tests.pop())
            .expect("converted suite must contain a post test");
        revm_fixture::Case { unit, test }
    }
}

pub(crate) mod revm_fixture {
    pub(crate) struct Case {
        pub(crate) unit: ::revm::statetest_types::TestUnit,
        pub(crate) test: ::revm::statetest_types::Test,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Env {
    current_coinbase: Address,
    #[serde(default)]
    current_difficulty: U256,
    current_gas_limit: U256,
    current_number: U256,
    current_timestamp: U256,
    #[serde(rename = "currentBaseFee", default)]
    current_base_fee: Option<U256>,
    #[serde(default)]
    current_random: Option<U256>,
    #[serde(default)]
    slot_number: Option<U256>,
}

impl Env {
    fn block(&self) -> BlockEnv {
        BlockEnv {
            number: self.current_number,
            beneficiary: self.current_coinbase,
            timestamp: self.current_timestamp,
            gas_limit: self.current_gas_limit,
            basefee: self.current_base_fee.unwrap_or_default(),
            difficulty: self.current_difficulty,
            prevrandao: self.current_random.unwrap_or_default(),
            slot_num: self.slot_number.unwrap_or_default(),
            ..BlockEnv::default()
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Account {
    balance: U256,
    code: Bytes,
    nonce: U256,
    storage: BTreeMap<U256, U256>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transaction {
    data: Bytes,
    gas_limit: U256,
    #[serde(default)]
    gas_price: Option<U256>,
    nonce: U256,
    sender: Address,
    #[serde(default, deserialize_with = "deserialize_maybe_empty")]
    to: Option<Address>,
    #[serde(default)]
    value: Option<U256>,
}

impl Transaction {
    fn envelope(&self, spec: SpecId) -> RecoveredTxEnvelope {
        let gas_limit = self.capped_gas_limit(spec);
        let tx = TxLegacy {
            chain_id: None,
            nonce: u64_value(self.nonce),
            gas_price: u128_value(self.gas_price.unwrap_or_default()),
            gas_limit,
            to: self.to.map_or(TxKind::Create, TxKind::Call),
            value: self.value.unwrap_or_default(),
            input: self.data.clone(),
        };
        RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, self.sender))
    }

    fn capped_gas_limit(&self, spec: SpecId) -> u64 {
        let version = Version::base(spec);
        u64_value(self.gas_limit).min(version.tx_gas_limit_cap)
    }
}

fn b256_from_u256(value: U256) -> B256 {
    B256::from(value.to_be_bytes())
}

fn revm_spec_name(spec: SpecId) -> &'static str {
    match spec {
        SpecId::FRONTIER => "Frontier",
        SpecId::HOMESTEAD => "Homestead",
        SpecId::TANGERINE => "EIP150",
        SpecId::SPURIOUS_DRAGON => "EIP158",
        SpecId::BYZANTIUM => "Byzantium",
        SpecId::PETERSBURG => "ConstantinopleFix",
        SpecId::ISTANBUL => "Istanbul",
        SpecId::BERLIN => "Berlin",
        SpecId::LONDON => "London",
        SpecId::MERGE => "Merge",
        SpecId::SHANGHAI => "Shanghai",
        SpecId::CANCUN => "Cancun",
        SpecId::PRAGUE => "Prague",
        SpecId::OSAKA => "Osaka",
        SpecId::AMSTERDAM => "Amsterdam",
        _ => panic!("unsupported benchmark spec: {spec:?}"),
    }
}

fn u128_value(value: U256) -> u128 {
    value.try_into().expect("fixture u128 value must fit")
}

fn u64_value(value: U256) -> u64 {
    value.try_into().expect("fixture u64 value must fit")
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
