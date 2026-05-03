use crate::{
    error::{TestError, TestErrorKind},
    types::{AccountInfo, Env, Test, TestSuite, TestUnit, TransactionParts, TxPartIndices},
};
use alloy_primitives::{Address, B256, Bytes, U256};
use evm2::{
    Evm, EvmVersion,
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{
        AccountInfo as EvmAccountInfo, InMemoryDB, logs_hash,
        transaction::{EvmError, ExecutionResult, Transaction},
    },
    interpreter::SpecId,
    registry::TxRegistry,
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
    result: Result<SpecOutcome, EvmError>,
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
    tx: &Transaction,
) -> Result<SpecOutcome, EvmError> {
    macro_rules! run {
        ($spec:ident) => {{
            let mut evm = Evm::<EvmVersion<(), { SpecId::$spec as u8 }>>::with_database(
                block,
                TxRegistry::new(),
                database,
            );
            let result = evm.execute(tx)?;
            Ok(spec_outcome(&evm, result))
        }};
    }
    match spec {
        SpecId::FRONTIER => run!(FRONTIER),
        SpecId::FRONTIER_THAWING => run!(FRONTIER_THAWING),
        SpecId::HOMESTEAD => run!(HOMESTEAD),
        SpecId::DAO_FORK => run!(DAO_FORK),
        SpecId::TANGERINE => run!(TANGERINE),
        SpecId::SPURIOUS_DRAGON => run!(SPURIOUS_DRAGON),
        SpecId::BYZANTIUM => run!(BYZANTIUM),
        SpecId::CONSTANTINOPLE => run!(CONSTANTINOPLE),
        SpecId::PETERSBURG => run!(PETERSBURG),
        SpecId::ISTANBUL => run!(ISTANBUL),
        SpecId::MUIR_GLACIER => run!(MUIR_GLACIER),
        SpecId::BERLIN => run!(BERLIN),
        SpecId::LONDON => run!(LONDON),
        SpecId::ARROW_GLACIER => run!(ARROW_GLACIER),
        SpecId::GRAY_GLACIER => run!(GRAY_GLACIER),
        SpecId::MERGE => run!(MERGE),
        SpecId::SHANGHAI => run!(SHANGHAI),
        SpecId::CANCUN => run!(CANCUN),
        SpecId::PRAGUE => run!(PRAGUE),
        SpecId::OSAKA => run!(OSAKA),
        SpecId::AMSTERDAM => run!(AMSTERDAM),
        _ => unreachable!("unknown statetest spec: {spec:?}"),
    }
}

fn spec_outcome<C>(evm: &Evm<C>, result: ExecutionResult) -> SpecOutcome
where
    C: evm2::config::EvmConfig<Database = InMemoryDB>,
{
    SpecOutcome {
        state_root: evm.state().state_root(),
        logs_root: logs_hash(evm.logs()),
        output: result.output,
        gas_used: result.gas_used,
        evm_result: format!("{:?}", result.stop),
    }
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
) -> Result<Transaction, TestErrorKind> {
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
    let gas_price = effective_gas_price(raw, base_fee)?;
    Ok(Transaction { caller, to: raw.to, nonce, gas_limit, gas_price, value, data })
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
