use alloy_consensus::{TxLegacy, transaction::Recovered};
#[cfg(feature = "jit")]
use alloy_primitives::Bytes;
use alloy_primitives::{B256, TxKind, U256};
use evm2::{
    SpecId, Version,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, TxEnvelope},
    evm::{AccountInfo, InMemoryDB},
};
use evm2_eest::{StateTestPost, StateTestSuite, StateTestUnit};
use std::{collections::HashMap, fs, path::PathBuf};

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
                Suite::parse(path, input)
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

#[derive(Debug)]
pub(crate) struct Suite {
    input: String,
    tests: StateTestSuite,
}

impl Suite {
    fn parse(path: PathBuf, input: String) -> Self {
        let tests = serde_json::from_str(&input).unwrap_or_else(|err| {
            panic!("failed to parse {} as state test fixture: {err}", path.display());
        });
        Self { input, tests }
    }

    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    pub(crate) fn case(&self, name: &str, spec: SpecId) -> Case<'_> {
        let unit = self.unit(name);
        let post = unit
            .post
            .iter()
            .filter(|(spec_name, _)| spec_name.to_spec_id() == Some(spec))
            .flat_map(|(_, posts)| posts)
            .next()
            .unwrap_or_else(|| panic!("fixture suite does not contain {name} post for {spec:?}"));
        Case { unit, post }
    }

    pub(crate) fn case_names(&self) -> impl Iterator<Item = &str> {
        self.tests.0.keys().map(String::as_str)
    }

    fn unit(&self, name: &str) -> &StateTestUnit {
        if let Some(unit) = self.tests.0.get(name) {
            return unit;
        }
        if self.tests.0.len() == 1 {
            return self.tests.0.values().next().expect("fixture must contain a case");
        }
        panic!("fixture suite does not contain benchmark case {name}");
    }
}

pub(crate) struct Case<'a> {
    unit: &'a StateTestUnit,
    post: &'a StateTestPost,
}

impl Case<'_> {
    pub(crate) fn block(&self) -> BlockEnv {
        let env = &self.unit.env;
        BlockEnv {
            number: env.current_number,
            beneficiary: env.current_coinbase,
            timestamp: env.current_timestamp,
            gas_limit: env.current_gas_limit,
            basefee: env.current_base_fee.unwrap_or_default(),
            difficulty: env.current_difficulty,
            prevrandao: env.current_random.map_or(U256::ZERO, b256_to_u256),
            slot_num: env.slot_number.unwrap_or_default(),
            ..BlockEnv::default()
        }
    }

    pub(crate) fn state(&self) -> InMemoryDB {
        let mut db = InMemoryDB::default();
        for (address, account) in &self.unit.pre {
            let mut info =
                AccountInfo::default().with_code(Bytecode::new_legacy(account.code.clone()));
            info.balance = account.balance;
            info.nonce = account.nonce;
            db.insert_account_info(address, info);

            for (key, value) in &account.storage {
                db.insert_account_storage(address, key, value);
            }
        }
        db
    }

    pub(crate) fn tx(&self, spec: SpecId) -> RecoveredTxEnvelope {
        let raw = &self.unit.transaction;
        let indexes = self.post.indexes;
        let gas_limit =
            raw.gas_limit.get(indexes.gas).copied().expect("fixture gas index must exist");
        let gas_limit = u64_value(gas_limit).min(Version::base(spec).tx_gas_limit_cap);
        let tx = TxLegacy {
            chain_id: None,
            nonce: u64_value(raw.nonce),
            gas_price: u128_value(raw.gas_price.unwrap_or_default()),
            gas_limit,
            to: raw.to.map_or(TxKind::Create, TxKind::Call),
            value: raw
                .value
                .get(indexes.value)
                .copied()
                .expect("fixture transaction value index must exist"),
            input: raw
                .data
                .get(indexes.data)
                .cloned()
                .expect("fixture transaction data index must exist"),
        };
        Recovered::new_unchecked(
            TxEnvelope::Legacy(tx),
            raw.sender.expect("benchmark transaction sender is required"),
        )
    }

    #[cfg(feature = "jit")]
    pub(crate) fn entry_bytecode(&self) -> Option<Bytes> {
        let target = self.unit.transaction.to?;
        let account = self.unit.pre.get(&target)?;
        (!account.code.is_empty()).then(|| account.code.clone())
    }

    #[cfg(feature = "jit")]
    pub(crate) fn compiled_accounts(&self) -> Vec<CompiledAccount> {
        self.unit
            .pre
            .values()
            .filter_map(|account| {
                if account.code.is_empty() {
                    return None;
                }
                let bytecode = Bytecode::new_legacy(account.code.clone());
                Some(CompiledAccount {
                    code_hash: bytecode.hash_slow(),
                    bytecode: account.code.clone(),
                })
            })
            .collect()
    }
}

fn b256_to_u256(value: B256) -> U256 {
    U256::from_be_bytes(value.0)
}

#[cfg(feature = "jit")]
pub(crate) struct CompiledAccount {
    pub(crate) code_hash: B256,
    pub(crate) bytecode: Bytes,
}

fn u128_value(value: U256) -> u128 {
    value.try_into().expect("fixture u128 value must fit")
}

fn u64_value(value: U256) -> u64 {
    value.try_into().expect("fixture u64 value must fit")
}
