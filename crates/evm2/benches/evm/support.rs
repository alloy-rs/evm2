use crate::cases::Bench;
use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::{
    Evm, EvmVersion,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
    interpreter::SpecId,
};
use serde::{Deserialize, Deserializer, de};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct PreparedBench {
    name: &'static str,
    spec: SpecId,
    block: BlockEnv,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
}

impl PreparedBench {
    pub(crate) fn load(bench: &Bench) -> Self {
        let raw: RawSuite = serde_json::from_str(bench.fixture).expect("fixture must parse");
        let raw = raw
            .0
            .get(bench.name)
            .or_else(|| raw.0.values().next())
            .expect("fixture must contain a case");
        let tx = raw.transaction.first().expect("fixture must contain a transaction");

        let mut db = InMemoryDB::default();
        for (address, account) in &raw.pre {
            let mut info =
                AccountInfo::default().with_code(Bytecode::new_legacy(account.code.clone()));
            info.balance = account.balance;
            info.nonce = u64_value(account.nonce);
            db.insert_account_info(*address, info);

            for (key, value) in &account.storage {
                db.insert_account_storage(*address, *key, *value);
            }
        }

        Self { name: bench.name, spec: bench.spec, block: raw.env.block(), db, tx: tx.envelope() }
    }

    pub(crate) fn sanity_check(&self) {
        match self.spec {
            SpecId::CANCUN => self.sanity_check_spec::<{ SpecId::CANCUN as u8 }>(),
            SpecId::OSAKA => self.sanity_check_spec::<{ SpecId::OSAKA as u8 }>(),
            spec => panic!("unsupported benchmark spec: {spec:?}"),
        }
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        match self.spec {
            SpecId::CANCUN => self.bench_spec::<{ SpecId::CANCUN as u8 }>(group),
            SpecId::OSAKA => self.bench_spec::<{ SpecId::OSAKA as u8 }>(group),
            spec => panic!("unsupported benchmark spec: {spec:?}"),
        }
    }

    fn sanity_check_spec<const SPEC: u8>(&self) {
        let mut runner = Runner::<SPEC>::new(self);
        let result = runner.run().expect("benchmark transaction must execute");
        assert!(result.status, "benchmark transaction failed: {:?}", result.stop);
    }

    fn bench_spec<const SPEC: u8>(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/transact", self.name), |b| {
            b.iter_batched(
                || Runner::<SPEC>::new(self),
                |mut runner| black_box(runner.run().expect("benchmark transaction must execute")),
                BatchSize::SmallInput,
            );
        });
    }
}

struct Runner<const SPEC: u8> {
    evm: Evm<EvmVersion<RecoveredTxEnvelope, SPEC>>,
    tx: RecoveredTxEnvelope,
}

impl<const SPEC: u8> Runner<SPEC> {
    fn new(prepared: &PreparedBench) -> Self {
        Self {
            evm: Evm::new(
                prepared.block,
                ethereum_tx_registry(),
                prepared.db.clone(),
                Default::default(),
            ),
            tx: prepared.tx.clone(),
        }
    }

    fn run(&mut self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        self.evm.transact(&self.tx)
    }
}

#[derive(Debug, Deserialize)]
struct RawSuite(BTreeMap<String, RawFixture>);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFixture {
    env: RawEnv,
    pre: BTreeMap<Address, RawAccount>,
    transaction: Vec<RawTransaction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEnv {
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

impl RawEnv {
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

#[derive(Debug, Deserialize)]
struct RawAccount {
    balance: U256,
    code: Bytes,
    nonce: U256,
    storage: BTreeMap<U256, U256>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTransaction {
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

impl RawTransaction {
    fn envelope(&self) -> RecoveredTxEnvelope {
        let tx = TxLegacy {
            chain_id: None,
            nonce: u64_value(self.nonce),
            gas_price: u128_value(self.gas_price.unwrap_or_default()),
            gas_limit: u64_value(self.gas_limit),
            to: self.to.map_or(TxKind::Create, TxKind::Call),
            value: self.value.unwrap_or_default(),
            input: self.data.clone(),
        };
        RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, self.sender))
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
