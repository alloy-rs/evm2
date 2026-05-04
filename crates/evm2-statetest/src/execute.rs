use crate::{
    error::{TestError, TestErrorKind},
    types::{AccountInfo, Env, Test, TestSuite, TestUnit, TransactionParts, TxPartIndices},
};
use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_trie::{
    TrieAccount,
    root::{state_root_unhashed, storage_root_unhashed},
};
use evm2::{
    BaseEvmTypes, Evm, EvmTypes, TxResult,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo as EvmAccountInfo, InMemoryDB, State, logs_hash},
    interpreter::SpecId,
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
            let mut evm = Evm::<BaseEvmTypes<RecoveredTxEnvelope>>::new(
                SpecId::$spec,
                block,
                ethereum_tx_registry(),
                database,
                Default::default(),
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
    spec: SpecId,
) -> SpecOutcome {
    SpecOutcome {
        state_root: state_root(evm.state(), spec),
        logs_root: logs_hash(evm.logs()),
        output: result.output,
        gas_used: result.gas_used,
        evm_result: format!("{:?}", result.stop),
    }
}

fn state_root(state: &State<InMemoryDB>, spec: SpecId) -> B256 {
    let mut addresses = Vec::from_iter(state.initial.accounts.keys().copied());
    addresses.extend(state.modified.keys().copied());
    addresses.sort_unstable();
    addresses.dedup();

    let accounts = addresses.into_iter().filter_map(|address| {
        let info = state.account_info(address)?;
        let storage = storage_for_root(state, address);
        if !include_account_in_root(state, address, &info, &storage, spec) {
            return None;
        }

        Some((
            address,
            TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                storage_root: storage_root_unhashed(storage),
                code_hash: info.code_hash,
            },
        ))
    });

    state_root_unhashed(accounts)
}

fn include_account_in_root(
    state: &State<InMemoryDB>,
    address: Address,
    info: &EvmAccountInfo,
    storage: &[(B256, U256)],
    spec: SpecId,
) -> bool {
    if !spec.enables(SpecId::SPURIOUS_DRAGON) {
        return state.initial.accounts.contains_key(&address)
            || state.modified.contains_key(&address);
    }

    if !info.is_empty() || !storage.is_empty() {
        return true;
    }

    // Approximate revm's `AccountState::None` case without tracking account state locally:
    // untouched empty accounts that existed before execution remain in the state trie.
    state.initial.accounts.contains_key(&address) && !state.modified.contains_key(&address)
}

fn storage_for_root(state: &State<InMemoryDB>, address: Address) -> Vec<(B256, U256)> {
    let mut storage = Vec::new();
    for ((storage_address, key), value) in &state.initial.storage {
        if *storage_address == address && !value.is_zero() {
            storage.push((B256::from(key.to_be_bytes()), *value));
        }
    }

    if let Some(account) = state.modified.get(&address) {
        for (key, value) in &account.storage {
            let key = B256::from(key.to_be_bytes());
            if let Some(existing) =
                storage.iter_mut().find(|(existing_key, _)| *existing_key == key)
            {
                existing.1 = value.current;
            } else if !value.current.is_zero() {
                storage.push((key, value.current));
            }
        }
    }

    storage.retain(|(_, value)| !value.is_zero());
    storage
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
