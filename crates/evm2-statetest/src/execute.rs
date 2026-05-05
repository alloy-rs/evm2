use crate::{
    error::{TestError, TestErrorKind},
    types::{AccountInfo, Env, Test, TestSuite, TestUnit, TransactionParts, TxPartIndices},
};
use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, Log, TxKind, U256, keccak256};
use alloy_trie::{
    TrieAccount,
    root::{state_root_unhashed, storage_root_unhashed},
};
use evm2::{
    BaseEvmTypes, Evm, EvmTypes, Precompiles, SpecId, TxResult,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo as EvmAccountInfo, InMemoryDB, StateChanges},
    registry::HandlerError,
};
use k256::ecdsa::SigningKey;
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path};

/// Per-spec execution outcome.
#[derive(Clone, Debug)]
pub(crate) struct SpecOutcome {
    /// Computed state root.
    pub(crate) state_root: B256,
    /// Computed logs root.
    pub(crate) logs_root: B256,
    /// Transaction output.
    pub(crate) output: Bytes,
    /// Gas used by the transaction.
    pub(crate) gas_used: u64,
    /// EVM result string.
    pub(crate) evm_result: String,
}

/// Execution options for a single suite.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExecuteConfig {
    /// Whether to print revm-style JSON outcome records.
    pub(crate) print_json_outcome: bool,
}

/// Executes a single state test JSON file using explicit execution options.
pub(crate) fn execute_test_suite(path: &Path, config: ExecuteConfig) -> Result<(), TestError> {
    let input = fs::read_to_string(path).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_str_with_config(path, &input, config)
}

/// Executes a loaded state test JSON file using explicit execution options.
pub(crate) fn execute_str_with_config(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
) -> Result<(), TestError> {
    let suite: TestSuite =
        serde_json::from_str(input).map_err(|err| TestError::unknown(path, err.into()))?;
    for (name, unit) in suite.0 {
        execute_unit(path, &name, unit, config)?;
    }
    Ok(())
}

fn execute_unit(
    path: &Path,
    name: &str,
    unit: TestUnit,
    config: ExecuteConfig,
) -> Result<(), TestError> {
    let state = parse_state(&unit.pre).map_err(|err| TestError::case(path, name, err))?;
    let block = parse_block(&unit.env);
    for (spec_name, posts) in &unit.post {
        let Some(spec) = spec_name.to_spec_id() else {
            continue;
        };
        for post in posts {
            let tx = match build_tx(&unit.transaction, &post.indexes, block.basefee) {
                Ok(tx) => tx,
                Err(_) if post.expect_exception.is_some() => {
                    continue;
                }
                Err(err) => return Err(TestError::case(path, name, err)),
            };
            let result = execute_spec(spec, block, state.clone(), &tx);
            validate_result(path, name, &unit, post, result, spec, config)?;
        }
    }
    Ok(())
}

fn validate_result(
    path: &Path,
    name: &str,
    unit: &TestUnit,
    post: &Test,
    result: Result<SpecOutcome, HandlerError>,
    spec: SpecId,
    config: ExecuteConfig,
) -> Result<(), TestError> {
    let error = match (&post.expect_exception, result) {
        (Some(_), Err(_)) => {
            if config.print_json_outcome {
                print_json_outcome(post, name, None, spec, None);
            }
            return Ok(());
        }
        (Some(_), Ok(outcome)) => Some(TestErrorKind::UnexpectedException {
            expected_exception: post.expect_exception.clone(),
            got_exception: None,
        })
        .inspect(|err| {
            if config.print_json_outcome {
                print_json_outcome(post, name, Some(&outcome), spec, Some(err));
            }
        }),
        (None, Err(err)) => Some(TestErrorKind::UnexpectedException {
            expected_exception: None,
            got_exception: Some(err.to_string()),
        })
        .inspect(|kind| {
            if config.print_json_outcome {
                print_json_outcome(post, name, None, spec, Some(kind));
            }
        }),
        (None, Ok(outcome)) => {
            let error = validate_outcome(unit, post, &outcome);
            if config.print_json_outcome {
                print_json_outcome(post, name, Some(&outcome), spec, error.as_ref());
            }
            if let Some(error) = error {
                return Err(TestError::case(path, name, error));
            }
            return Ok(());
        }
    };

    if let Some(error) = error {
        return Err(TestError::case(path, name, error));
    }
    Ok(())
}

fn validate_outcome(unit: &TestUnit, post: &Test, outcome: &SpecOutcome) -> Option<TestErrorKind> {
    if let Some(expected) = unit.out.as_ref()
        && expected != &outcome.output
    {
        return Some(TestErrorKind::UnexpectedOutput {
            expected_output: Some(expected.clone()),
            got_output: Some(outcome.output.clone()),
        });
    }
    if outcome.logs_root != post.logs {
        return Some(TestErrorKind::LogsRootMismatch {
            got: outcome.logs_root,
            expected: post.logs,
        });
    }
    if outcome.state_root != post.hash {
        return Some(TestErrorKind::StateRootMismatch {
            got: outcome.state_root,
            expected: post.hash,
        });
    }
    None
}

fn print_json_outcome(
    test: &Test,
    test_name: &str,
    outcome: Option<&SpecOutcome>,
    spec: SpecId,
    error: Option<&TestErrorKind>,
) {
    let output = outcome.map_or_else(Bytes::new, |outcome| outcome.output.clone());
    let gas_used = outcome.map_or(0, |outcome| outcome.gas_used);
    let logs_root = outcome.map_or(B256::ZERO, |outcome| outcome.logs_root);
    let state_root = outcome.map_or(B256::ZERO, |outcome| outcome.state_root);
    let evm_result =
        outcome.map_or_else(|| "Error".to_string(), |outcome| outcome.evm_result.clone());
    let value = json!({
        "stateRoot": state_root,
        "logsRoot": logs_root,
        "output": output,
        "gasUsed": gas_used,
        "pass": error.is_none(),
        "errorMsg": error.map(ToString::to_string).unwrap_or_default(),
        "evmResult": evm_result,
        "postLogsHash": logs_root,
        "fork": format!("{spec:?}"),
        "test": test_name,
        "d": test.indexes.data,
        "g": test.indexes.gas,
        "v": test.indexes.value,
    });
    eprintln!("{value}");
}

fn execute_spec(
    spec: SpecId,
    block: BlockEnv,
    database: InMemoryDB,
    tx: &RecoveredTxEnvelope,
) -> Result<SpecOutcome, HandlerError> {
    macro_rules! run {
        ($spec:ident) => {{
            let spec = SpecId::$spec;
            let mut evm = Evm::<BaseEvmTypes<RecoveredTxEnvelope>>::new(
                spec,
                block,
                ethereum_tx_registry(),
                database,
                Precompiles::base(spec),
            );
            let result = evm.transact(tx)?;
            Ok(spec_outcome(&evm, result, spec))
        }};
    }
    match spec {
        SpecId::FRONTIER => run!(FRONTIER),
        SpecId::HOMESTEAD => run!(HOMESTEAD),
        SpecId::TANGERINE => run!(TANGERINE),
        SpecId::SPURIOUS_DRAGON => run!(SPURIOUS_DRAGON),
        SpecId::BYZANTIUM => run!(BYZANTIUM),
        SpecId::PETERSBURG => run!(PETERSBURG),
        SpecId::ISTANBUL => run!(ISTANBUL),
        SpecId::BERLIN => run!(BERLIN),
        SpecId::LONDON => run!(LONDON),
        SpecId::MERGE => run!(MERGE),
        SpecId::SHANGHAI => run!(SHANGHAI),
        SpecId::CANCUN => run!(CANCUN),
        SpecId::PRAGUE => run!(PRAGUE),
        SpecId::OSAKA => run!(OSAKA),
        SpecId::AMSTERDAM => run!(AMSTERDAM),
        _ => unreachable!("unknown statetest spec: {spec:?}"),
    }
}

fn spec_outcome<T: EvmTypes<Database = InMemoryDB>>(
    evm: &Evm<T>,
    result: TxResult,
    _spec: SpecId,
) -> SpecOutcome {
    SpecOutcome {
        state_root: state_root(evm.state().initial(), &result.state_changes),
        logs_root: logs_hash(&result.state_changes.logs),
        output: result.output,
        gas_used: result.gas_used,
        evm_result: format!("{:?}", result.stop),
    }
}

fn logs_hash(logs: &[Log]) -> B256 {
    let mut out = Vec::with_capacity(alloy_rlp::list_length(logs));
    alloy_rlp::encode_list(logs, &mut out);
    keccak256(out)
}

fn state_root(pre: &InMemoryDB, changes: &StateChanges) -> B256 {
    let post = apply_state_changes(pre, changes);
    let accounts = post.accounts.iter().map(|(&address, info)| {
        let storage = storage_for_root(&post, address);
        (
            address,
            TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                storage_root: storage_root_unhashed(storage),
                code_hash: info.code_hash,
            },
        )
    });

    state_root_unhashed(accounts)
}

fn apply_state_changes(pre: &InMemoryDB, changes: &StateChanges) -> InMemoryDB {
    let mut post = pre.clone();
    for (&code_hash, code) in &changes.code {
        post.contracts.insert(code_hash, code.clone());
    }
    for (&address, storage) in &changes.storage {
        if storage.wipe {
            post.storage.retain(|(storage_address, _), _| *storage_address != address);
        }
        for (&key, change) in &storage.slots {
            if change.current.is_zero() {
                post.storage.remove(&(address, key));
            } else {
                post.storage.insert((address, key), change.current);
            }
        }
    }
    for (&address, change) in &changes.accounts {
        match &change.current {
            Some(info) => post.insert_account_info(address, info.clone()),
            None => {
                post.accounts.remove(&address);
                post.storage.retain(|(storage_address, _), _| *storage_address != address);
            }
        }
    }
    post
}

fn storage_for_root(state: &InMemoryDB, address: Address) -> Vec<(B256, U256)> {
    state
        .storage
        .iter()
        .filter_map(|(&(storage_address, key), &value)| {
            (storage_address == address && !value.is_zero())
                .then_some((B256::from(key.to_be_bytes()), value))
        })
        .collect()
}

fn parse_state(pre: &BTreeMap<Address, AccountInfo>) -> Result<InMemoryDB, TestErrorKind> {
    let mut database = InMemoryDB::default();
    for (address, account) in pre {
        let mut info =
            EvmAccountInfo::default().with_code(Bytecode::new_legacy(account.code.clone()));
        info.nonce = account.nonce;
        info.balance = account.balance;
        database.insert_account_info(*address, info);
        for (key, value) in &account.storage {
            database.insert_account_storage(*address, *key, *value);
        }
    }
    Ok(database)
}

fn parse_block(env: &Env) -> BlockEnv {
    BlockEnv {
        number: env.current_number,
        beneficiary: env.current_coinbase,
        timestamp: env.current_timestamp,
        gas_limit: env.current_gas_limit,
        basefee: env.current_base_fee.unwrap_or_default(),
        difficulty: env.current_difficulty,
        prevrandao: env
            .current_random
            .map_or(U256::ZERO, |random| U256::from_be_slice(random.as_slice())),
        slot_num: env.slot_number.unwrap_or_default(),
        ..BlockEnv::default()
    }
}

fn build_tx(
    raw: &TransactionParts,
    indexes: &TxPartIndices,
    base_fee: U256,
) -> Result<RecoveredTxEnvelope, TestErrorKind> {
    let caller = match raw.sender {
        Some(sender) => sender,
        None => recover_address(raw.secret_key.as_slice())
            .ok_or(TestErrorKind::UnknownPrivateKey(raw.secret_key))?,
    };
    let data = raw.data.get(indexes.data).ok_or(TestErrorKind::BadIndex("data"))?.clone();
    let gas_limit = raw
        .gas_limit
        .get(indexes.gas)
        .ok_or(TestErrorKind::BadIndex("gas"))?
        .try_into()
        .map_err(|_| TestErrorKind::Overflow("gasLimit"))?;
    let value = *raw.value.get(indexes.value).ok_or(TestErrorKind::BadIndex("value"))?;
    let nonce = raw.nonce.try_into().map_err(|_| TestErrorKind::Overflow("nonce"))?;
    let gas_price = effective_gas_price(raw, base_fee)?
        .try_into()
        .map_err(|_| TestErrorKind::Overflow("gasPrice"))?;
    let tx = TxLegacy {
        chain_id: None,
        nonce,
        gas_price,
        gas_limit,
        to: TxKind::from(raw.to),
        value,
        input: data,
    };
    Ok(RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, caller)))
}

fn effective_gas_price(raw: &TransactionParts, base_fee: U256) -> Result<U256, TestErrorKind> {
    if let Some(gas_price) = raw.gas_price {
        return Ok(gas_price);
    }

    let Some(max_fee_per_gas) = raw.max_fee_per_gas else {
        return Ok(U256::ZERO);
    };
    if max_fee_per_gas < base_fee {
        return Err(TestErrorKind::FeeCapLessThanBaseFee { max_fee_per_gas, base_fee });
    }

    let priority_fee = raw.max_priority_fee_per_gas.unwrap_or_default();
    Ok(max_fee_per_gas.min(base_fee.saturating_add(priority_fee)))
}

fn recover_address(private_key: &[u8]) -> Option<Address> {
    let key = SigningKey::from_slice(private_key).ok()?;
    let public_key = key.verifying_key().to_encoded_point(false);
    Some(Address::from_raw_public_key(&public_key.as_bytes()[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::LogData;

    #[test]
    fn logs_hash_matches_empty_logs() {
        assert_eq!(logs_hash(&[]), keccak256([alloy_rlp::EMPTY_LIST_CODE]));
    }

    #[test]
    fn logs_hash_hashes_logs() {
        let log = Log {
            address: Address::from([0x22; 20]),
            data: LogData::new_unchecked(vec![B256::with_last_byte(1)], Bytes::from_static(&[2])),
        };

        assert_ne!(logs_hash(&[log]), B256::ZERO);
    }

    #[test]
    fn effective_gas_price_uses_legacy_gas_price() {
        let tx = TransactionParts {
            gas_price: Some(U256::from(7)),
            max_fee_per_gas: Some(U256::from(10)),
            max_priority_fee_per_gas: Some(U256::from(2)),
            ..TransactionParts::default()
        };

        assert_eq!(effective_gas_price(&tx, U256::from(100)).unwrap(), U256::from(7));
    }

    #[test]
    fn effective_gas_price_caps_priority_fee() {
        let tx = TransactionParts {
            max_fee_per_gas: Some(U256::from(10)),
            max_priority_fee_per_gas: Some(U256::from(3)),
            ..TransactionParts::default()
        };

        assert_eq!(effective_gas_price(&tx, U256::from(8)).unwrap(), U256::from(10));
    }

    #[test]
    fn effective_gas_price_rejects_fee_cap_below_base_fee() {
        let tx = TransactionParts {
            max_fee_per_gas: Some(U256::from(7)),
            ..TransactionParts::default()
        };

        let err = effective_gas_price(&tx, U256::from(8)).unwrap_err();
        assert!(matches!(
            err,
            TestErrorKind::FeeCapLessThanBaseFee {
                max_fee_per_gas,
                base_fee
            } if max_fee_per_gas == U256::from(7) && base_fee == U256::from(8)
        ));
    }
}
