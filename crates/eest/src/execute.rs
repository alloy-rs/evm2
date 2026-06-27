#[cfg(feature = "jit")]
use crate::compiled::{self, FileSummary};
use crate::{
    error::{TestError, TestErrorKind},
    execution::ExecutionResources,
    filter::EntryPoint,
    fixture_io,
    forks::is_fork_skipped,
    state::{
        insert_account_with_storage, parse_bytecode, storage_for_root, system_contract_has_code,
    },
    tx::{TxFields, build_recovered_tx, recover_address, rpc_access_list, signed_authorizations},
    types::{AccountInfo, Env, Test, TestSuite, TestUnit, TransactionParts, TxPartIndices},
};
use alloy_eips::{eip4844, eip7691};
use alloy_primitives::{Address, B256, Bytes, Log, TxKind, U256, keccak256};
use alloy_rpc_types_eth::AccessList as RpcAccessList;
use alloy_trie::{
    TrieAccount,
    root::{state_root_unhashed, storage_root_unhashed},
};
use anstyle::{AnsiColor, Color, Style};
use evm2::{
    BaseEvmTypes, Evm, EvmTypes, Precompiles, SpecId, TxResult,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{
        AccountInfo as EvmAccountInfo, BEACON_ROOTS_ADDRESS, DbStats, DbStatsCounts,
        HISTORY_STORAGE_ADDRESS, InMemoryDB,
    },
    registry::HandlerError,
};
use serde_json::json;
#[cfg(feature = "jit")]
use std::path::PathBuf;
use std::{collections::BTreeMap, path::Path};

pub use crate::execution::ExecutionMode;

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
pub struct ExecuteConfig {
    /// Whether to print JSON outcome records.
    pub print_json_outcome: bool,
    /// Execution backend.
    pub mode: ExecutionMode,
    /// Whether to print database method call counts.
    pub db_stats: bool,
}

/// Per-file execution summary.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExecuteSummary {
    /// Number of executed test units.
    pub executed: usize,
    /// Number of test units skipped by the entrypoint filter.
    pub skipped: usize,
}

/// Executes a single state test JSON file using explicit execution options.
pub(crate) fn execute_test_suite(path: &Path, config: ExecuteConfig) -> Result<(), TestError> {
    let input =
        fixture_io::read_to_string(path).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_str_with_config(path, &input, config).map(|_| ())
}

/// Executes multiple state test JSON files using one shared execution resource set.
#[cfg(feature = "jit")]
pub(crate) fn execute_test_suites(
    paths: &[PathBuf],
    config: ExecuteConfig,
) -> Result<ExecuteSummary, TestError> {
    let error_path = paths.first().map_or_else(|| Path::new("state tests"), PathBuf::as_path);
    let resources = ExecutionResources::new(config.mode)
        .map_err(|err| TestError::unknown(error_path, err.into()))?;
    let summary = compiled::run_files(paths.to_vec(), resources, move |path, resources| {
        let input = fixture_io::read_to_string(&path)
            .map_err(|err| TestError::unknown(path.as_path(), err.into()))?;
        let mut db_stats_counts = DbStatsCounts::default();
        let file_summary = execute_str_with_resources(
            &path,
            &input,
            config,
            &EntryPoint::default(),
            &resources,
            &mut db_stats_counts,
        )?;
        Ok(FileSummary {
            executed: file_summary.executed,
            skipped: file_summary.skipped,
            db_stats_counts,
        })
    })?;

    if config.db_stats {
        print_db_stats(summary.db_stats_counts);
    }

    Ok(ExecuteSummary { executed: summary.executed, skipped: summary.skipped })
}

/// Executes a loaded state test JSON file using explicit execution options.
pub fn execute_str_with_config(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
) -> Result<ExecuteSummary, TestError> {
    execute_str_with_filter(path, input, config, &EntryPoint::default())
}

/// Executes a loaded state test JSON file, selecting test units by entrypoint.
pub fn execute_str_with_filter(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
    entrypoint: &EntryPoint,
) -> Result<ExecuteSummary, TestError> {
    let resources =
        ExecutionResources::new(config.mode).map_err(|err| TestError::unknown(path, err.into()))?;
    let mut db_stats_counts = DbStatsCounts::default();
    let summary = execute_str_with_resources(
        path,
        input,
        config,
        entrypoint,
        &resources,
        &mut db_stats_counts,
    )?;
    if config.db_stats {
        print_db_stats(db_stats_counts);
    }
    Ok(summary)
}

fn execute_str_with_resources(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
    entrypoint: &EntryPoint,
    resources: &ExecutionResources,
    db_stats_counts: &mut DbStatsCounts,
) -> Result<ExecuteSummary, TestError> {
    let suite: TestSuite =
        serde_json::from_str(input).map_err(|err| TestError::unknown(path, err.into()))?;
    let mut summary = ExecuteSummary::default();
    for (name, unit) in suite.0 {
        if !entrypoint.matches(&name) {
            summary.skipped += 1;
            continue;
        }
        execute_unit(path, &name, unit, config, resources, db_stats_counts)?;
        summary.executed += 1;
    }
    Ok(summary)
}

fn execute_unit(
    path: &Path,
    name: &str,
    unit: TestUnit,
    config: ExecuteConfig,
    resources: &ExecutionResources,
    db_stats_counts: &mut DbStatsCounts,
) -> Result<(), TestError> {
    let state = parse_state(&unit.pre).map_err(|err| TestError::case(path, name, err))?;
    for (spec_name, posts) in &unit.post {
        let Some(spec) = spec_name.to_spec_id() else {
            continue;
        };
        if is_fork_skipped(spec) {
            continue;
        }
        let block = parse_block(&unit.env, spec);
        for post in posts {
            let tx = match build_tx(&unit.transaction, &post.indexes, unit.env.current_chain_id) {
                Ok(tx) => tx,
                Err(_) if post.expect_exception.is_some() => {
                    continue;
                }
                Err(err) => return Err(TestError::case(path, name, err)),
            };
            let result = execute_spec(
                spec,
                block,
                state.clone(),
                &tx,
                &unit.env,
                resources,
                if config.db_stats { Some(&mut *db_stats_counts) } else { None },
            );
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
    env: &Env,
    resources: &ExecutionResources,
    db_stats_counts: Option<&mut DbStatsCounts>,
) -> Result<SpecOutcome, HandlerError> {
    let db_stats = db_stats_counts.is_some();
    let mut evm = if db_stats {
        Evm::<BaseEvmTypes>::new(
            spec,
            block,
            ethereum_tx_registry(spec),
            DbStats::new(database.clone()),
            Precompiles::base(spec),
        )
    } else {
        Evm::<BaseEvmTypes>::new(
            spec,
            block,
            ethereum_tx_registry(spec),
            database.clone(),
            Precompiles::base(spec),
        )
    };
    resources.configure_evm(&mut evm);
    let mut post = database;
    pre_block_system_calls(&mut evm, &mut post, spec, env);
    let Ok(result) = evm.transact(tx)?.commit_with(&mut post);
    if let Some(counts) = db_stats_counts
        && let Some(stats) = evm.database_as::<DbStats<InMemoryDB>>()
    {
        *counts += stats.counts();
    }
    Ok(spec_outcome(post, result))
}

fn print_db_stats(counts: DbStatsCounts) {
    let style = db_stats_style();
    eprintln!("{style}db stats{style:#}: get_account={}", counts.get_account);
    eprintln!("{style}db stats{style:#}: get_code_by_hash={}", counts.get_code_by_hash);
    eprintln!("{style}db stats{style:#}: get_storage={}", counts.get_storage);
    eprintln!(
        "{style}db stats{style:#}: get_storage_same_address_repeats={}",
        counts.get_storage_same_address_repeats
    );
    eprintln!(
        "{style}db stats{style:#}: get_storage_same_address_longest_streak={}",
        counts.get_storage_same_address_longest_streak
    );
    eprintln!("{style}db stats{style:#}: get_block_hash={}", counts.get_block_hash);
    eprintln!("{style}db stats{style:#}: error={}", counts.error);
}

#[inline]
const fn db_stats_style() -> Style {
    Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightCyan))).bold()
}

fn spec_outcome(post: InMemoryDB, result: TxResult) -> SpecOutcome {
    let gas_used = result.tx_gas_used();
    SpecOutcome {
        state_root: state_root_from_database(&post),
        logs_root: logs_hash(&result.logs),
        output: result.output,
        gas_used,
        evm_result: format!("{:?}", result.stop),
    }
}

fn pre_block_system_calls<T: EvmTypes<Host = Evm<T>>>(
    evm: &mut Evm<T>,
    post: &mut InMemoryDB,
    spec: SpecId,
    env: &Env,
) {
    if env.current_number.is_zero() {
        return;
    }

    if spec.enables(SpecId::PRAGUE)
        && let Some(previous_hash) = env.previous_hash
        && system_contract_has_code(post, HISTORY_STORAGE_ADDRESS)
    {
        commit_system_call(
            evm,
            post,
            HISTORY_STORAGE_ADDRESS,
            Bytes::copy_from_slice(previous_hash.as_slice()),
        );
    }
    if spec.enables(SpecId::CANCUN)
        && let Some(beacon_root) = env.current_beacon_root
        && system_contract_has_code(post, BEACON_ROOTS_ADDRESS)
    {
        commit_system_call(
            evm,
            post,
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(beacon_root.as_slice()),
        );
    }
}

fn commit_system_call<T: EvmTypes<Host = Evm<T>>>(
    evm: &mut Evm<T>,
    post: &mut InMemoryDB,
    address: Address,
    data: Bytes,
) {
    let executed = evm.system_call(address, data);
    assert!(executed.result().status, "pre-block system call failed: {address}");
    let Ok(_) = executed.commit_with(post);
}

fn logs_hash(logs: &[Log]) -> B256 {
    let mut out = Vec::with_capacity(alloy_rlp::list_length(logs));
    alloy_rlp::encode_list(logs, &mut out);
    keccak256(out)
}

fn state_root_from_database(state: &InMemoryDB) -> B256 {
    let accounts = state.cache.accounts.iter().filter_map(|(&address, info)| {
        let info = info.as_ref()?;
        let storage = storage_for_root(state, address);
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

fn parse_state(pre: &BTreeMap<Address, AccountInfo>) -> Result<InMemoryDB, TestErrorKind> {
    let mut database = InMemoryDB::default();
    for (address, account) in pre {
        let mut info = EvmAccountInfo::default().with_code(parse_bytecode(account.code.clone()));
        info.nonce = account.nonce;
        info.balance = account.balance;
        insert_account_with_storage(
            &mut database,
            *address,
            info,
            account.storage.iter().map(|(&key, &value)| (key, value)),
        );
    }
    Ok(database)
}

fn parse_block(env: &Env, spec: SpecId) -> BlockEnv {
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
        blob_basefee: env
            .current_excess_blob_gas
            .map_or(U256::ONE, |excess| U256::from(blob_basefee(excess, spec))),
        slot_num: env.slot_number.unwrap_or_default(),
        ext: (),
        _non_exhaustive: (),
    }
}

fn blob_basefee(excess_blob_gas: U256, spec: SpecId) -> u128 {
    let excess_blob_gas = excess_blob_gas.saturating_to::<u64>();
    // EIP-4844 defines blob base fee with fake exponential; EIP-7691 changes the
    // update fraction from Prague.
    if spec.enables(SpecId::PRAGUE) {
        eip7691::calc_blob_gasprice(excess_blob_gas)
    } else {
        eip4844::fake_exponential(
            eip4844::BLOB_TX_MIN_BLOB_GASPRICE,
            excess_blob_gas as u128,
            eip4844::BLOB_GASPRICE_UPDATE_FRACTION,
        )
    }
}

fn build_tx(
    raw: &TransactionParts,
    indexes: &TxPartIndices,
    chain_id: Option<U256>,
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

    Ok(build_recovered_tx(TxFields {
        tx_type: raw.tx_type,
        caller,
        kind: TxKind::from(raw.to),
        data,
        gas_limit,
        nonce,
        value,
        chain_id,
        gas_price: raw.gas_price,
        max_fee_per_gas: raw.max_fee_per_gas,
        max_priority_fee_per_gas: raw.max_priority_fee_per_gas,
        access_list: access_list(raw, indexes.data)?,
        authorization_list: signed_authorizations(raw.authorization_list.as_deref())?,
        blob_versioned_hashes: raw.blob_versioned_hashes.clone(),
        max_fee_per_blob_gas: raw.max_fee_per_blob_gas,
    })?)
}

fn access_list(
    raw: &TransactionParts,
    access_list_index: usize,
) -> Result<Option<RpcAccessList>, TestErrorKind> {
    if matches!(raw.tx_type, Some(0)) {
        return Ok(None);
    }
    let Some(access_list) = raw.access_lists.get(access_list_index).cloned().flatten() else {
        return Ok(matches!(raw.tx_type, Some(1)).then(RpcAccessList::default));
    };
    Ok(Some(rpc_access_list(access_list.iter())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::LogData;
    #[cfg(feature = "jit")]
    use evm2::interpreter::op;

    #[cfg(feature = "jit")]
    const BYTECODE_RET42: &[u8] =
        &[op::PUSH1, 0x42, op::PUSH0, op::MSTORE, op::PUSH1, 0x20, op::PUSH0, op::RETURN];

    #[cfg(feature = "jit")]
    fn call_value_to(to: Address) -> Bytes {
        let mut code = Vec::with_capacity(33);
        code.extend([op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH1, 1, op::PUSH20]);
        code.extend_from_slice(to.as_slice());
        code.extend([op::PUSH2, 0xc3, 0x50, op::CALL, op::STOP]);
        Bytes::from(code)
    }

    #[cfg(feature = "jit")]
    fn create_value() -> Bytes {
        Bytes::from_static(&[op::PUSH0, op::PUSH0, op::PUSH1, 1, op::CREATE, op::STOP])
    }

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
    fn build_tx_builds_legacy_transaction() {
        let caller = Address::from([0x11; 20]);
        let raw = TransactionParts {
            data: vec![Bytes::from_static(&[0xaa])],
            gas_limit: vec![U256::from(21_000)],
            gas_price: Some(U256::from(7)),
            sender: Some(caller),
            to: Some(Address::from([0x22; 20])),
            value: vec![U256::from(3)],
            ..TransactionParts::default()
        };

        let tx = build_tx(&raw, &TxPartIndices { data: 0, gas: 0, value: 0 }, None).unwrap();

        let RecoveredTxEnvelope::Legacy(tx) = tx else {
            panic!("expected legacy transaction");
        };
        assert_eq!(tx.signer(), caller);
        assert_eq!(tx.inner().gas_price, 7);
    }

    #[test]
    fn build_tx_builds_eip2930_transaction() {
        let caller = Address::from([0x11; 20]);
        let access_address = Address::from([0x33; 20]);
        let raw = TransactionParts {
            tx_type: Some(1),
            data: vec![Bytes::new()],
            gas_limit: vec![U256::from(25_300)],
            gas_price: Some(U256::from(7)),
            sender: Some(caller),
            to: Some(Address::from([0x22; 20])),
            value: vec![U256::ZERO],
            access_lists: vec![Some(vec![crate::tx::AccessListItem {
                address: access_address,
                storage_keys: vec![B256::with_last_byte(1)],
            }])],
            ..TransactionParts::default()
        };

        let tx = build_tx(&raw, &TxPartIndices { data: 0, gas: 0, value: 0 }, None).unwrap();

        let RecoveredTxEnvelope::Eip2930(tx) = tx else {
            panic!("expected EIP-2930 transaction");
        };
        assert_eq!(tx.signer(), caller);
        assert_eq!(tx.inner().access_list[0].address, access_address);
    }

    #[test]
    fn build_tx_uses_indexed_access_list() {
        let caller = Address::from([0x11; 20]);
        let first_address = Address::from([0x33; 20]);
        let second_address = Address::from([0x44; 20]);
        let raw = TransactionParts {
            tx_type: Some(1),
            data: vec![Bytes::new(), Bytes::from_static(&[0xaa])],
            gas_limit: vec![U256::from(25_300)],
            gas_price: Some(U256::from(7)),
            sender: Some(caller),
            to: Some(Address::from([0x22; 20])),
            value: vec![U256::ZERO],
            access_lists: vec![
                Some(vec![crate::tx::AccessListItem {
                    address: first_address,
                    storage_keys: vec![B256::with_last_byte(1)],
                }]),
                Some(vec![crate::tx::AccessListItem {
                    address: second_address,
                    storage_keys: vec![B256::with_last_byte(2)],
                }]),
            ],
            ..TransactionParts::default()
        };

        let tx = build_tx(&raw, &TxPartIndices { data: 1, gas: 0, value: 0 }, None).unwrap();

        let RecoveredTxEnvelope::Eip2930(tx) = tx else {
            panic!("expected EIP-2930 transaction");
        };
        assert_eq!(tx.inner().access_list[0].address, second_address);
    }

    #[cfg(feature = "jit")]
    fn execute_simple_call(mode: ExecutionMode) -> SpecOutcome {
        let caller = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);
        let env = Env {
            current_chain_id: Some(U256::ONE),
            current_coinbase: Address::ZERO,
            current_difficulty: U256::ZERO,
            current_gas_limit: U256::from(30_000_000),
            current_number: U256::ZERO,
            current_timestamp: U256::ZERO,
            current_base_fee: Some(U256::ZERO),
            previous_hash: None,
            current_random: None,
            current_beacon_root: None,
            current_withdrawals_root: None,
            current_excess_blob_gas: None,
            slot_number: None,
        };
        let mut pre = BTreeMap::new();
        pre.insert(
            caller,
            AccountInfo {
                balance: U256::from(1_000_000_000),
                code: Bytes::new(),
                nonce: 0,
                storage: BTreeMap::new(),
            },
        );
        pre.insert(
            target,
            AccountInfo {
                balance: U256::ZERO,
                code: Bytes::copy_from_slice(BYTECODE_RET42),
                nonce: 0,
                storage: BTreeMap::new(),
            },
        );
        let tx = build_tx(
            &TransactionParts {
                data: vec![Bytes::new()],
                gas_limit: vec![U256::from(100_000)],
                gas_price: Some(U256::ZERO),
                sender: Some(caller),
                to: Some(target),
                value: vec![U256::ZERO],
                ..TransactionParts::default()
            },
            &TxPartIndices { data: 0, gas: 0, value: 0 },
            env.current_chain_id,
        )
        .unwrap();
        let resources = ExecutionResources::new(mode).unwrap();
        execute_spec(
            SpecId::CANCUN,
            parse_block(&env, SpecId::CANCUN),
            parse_state(&pre).unwrap(),
            &tx,
            &env,
            &resources,
            None,
        )
        .unwrap()
    }

    #[cfg(feature = "jit")]
    fn execute_amsterdam_target(mode: ExecutionMode, target_code: Bytes) -> SpecOutcome {
        let caller = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);
        let coinbase = Address::from([0xcb; 20]);
        let env = Env {
            current_chain_id: Some(U256::ONE),
            current_coinbase: coinbase,
            current_difficulty: U256::ZERO,
            current_gas_limit: U256::from(30_000_000),
            current_number: U256::ZERO,
            current_timestamp: U256::ZERO,
            current_base_fee: Some(U256::ZERO),
            previous_hash: None,
            current_random: None,
            current_beacon_root: None,
            current_withdrawals_root: None,
            current_excess_blob_gas: None,
            slot_number: None,
        };
        let mut pre = BTreeMap::new();
        pre.insert(
            caller,
            AccountInfo {
                balance: U256::from(1_000_000_000),
                code: Bytes::new(),
                nonce: 0,
                storage: BTreeMap::new(),
            },
        );
        pre.insert(
            target,
            AccountInfo {
                balance: U256::from(10),
                code: target_code,
                nonce: 0,
                storage: BTreeMap::new(),
            },
        );
        let tx = build_tx(
            &TransactionParts {
                data: vec![Bytes::new()],
                gas_limit: vec![U256::from(200_000)],
                gas_price: Some(U256::ONE),
                sender: Some(caller),
                to: Some(target),
                value: vec![U256::ZERO],
                ..TransactionParts::default()
            },
            &TxPartIndices { data: 0, gas: 0, value: 0 },
            env.current_chain_id,
        )
        .unwrap();
        let resources = ExecutionResources::new(mode).unwrap();
        execute_spec(
            SpecId::AMSTERDAM,
            parse_block(&env, SpecId::AMSTERDAM),
            parse_state(&pre).unwrap(),
            &tx,
            &env,
            &resources,
            None,
        )
        .unwrap()
    }

    #[cfg(feature = "jit")]
    fn execute_amsterdam_value_call_to_empty_coinbase(mode: ExecutionMode) -> SpecOutcome {
        execute_amsterdam_target(mode, call_value_to(Address::from([0xcb; 20])))
    }

    #[cfg(feature = "jit")]
    fn execute_amsterdam_value_create(mode: ExecutionMode) -> SpecOutcome {
        execute_amsterdam_target(mode, create_value())
    }

    #[cfg(feature = "jit")]
    #[test]
    fn jit_and_aot_modes_match_interpreter_for_simple_call() {
        let interpreter = execute_simple_call(ExecutionMode::Interpreter);
        let jit = execute_simple_call(ExecutionMode::Jit);
        let aot = execute_simple_call(ExecutionMode::Aot);

        assert_eq!(jit.output, interpreter.output);
        assert_eq!(aot.output, interpreter.output);
        assert_eq!(jit.state_root, interpreter.state_root);
        assert_eq!(aot.state_root, interpreter.state_root);
        assert_eq!(jit.logs_root, interpreter.logs_root);
        assert_eq!(aot.logs_root, interpreter.logs_root);
        assert_eq!(jit.gas_used, interpreter.gas_used);
        assert_eq!(aot.gas_used, interpreter.gas_used);
        assert_eq!(interpreter.output.len(), 32);
        assert_eq!(interpreter.output[31], 0x42);
    }

    #[cfg(feature = "jit")]
    #[test]
    fn jit_and_aot_modes_match_interpreter_for_amsterdam_value_call_to_empty_coinbase() {
        let interpreter =
            execute_amsterdam_value_call_to_empty_coinbase(ExecutionMode::Interpreter);
        let jit = execute_amsterdam_value_call_to_empty_coinbase(ExecutionMode::Jit);
        let aot = execute_amsterdam_value_call_to_empty_coinbase(ExecutionMode::Aot);

        assert_eq!(jit.output, interpreter.output);
        assert_eq!(aot.output, interpreter.output);
        assert_eq!(jit.state_root, interpreter.state_root);
        assert_eq!(aot.state_root, interpreter.state_root);
        assert_eq!(jit.logs_root, interpreter.logs_root);
        assert_eq!(aot.logs_root, interpreter.logs_root);
        assert_eq!(jit.gas_used, interpreter.gas_used);
        assert_eq!(aot.gas_used, interpreter.gas_used);
        assert_eq!(jit.evm_result, interpreter.evm_result);
        assert_eq!(aot.evm_result, interpreter.evm_result);
    }

    #[cfg(feature = "jit")]
    #[test]
    fn jit_and_aot_modes_match_interpreter_for_amsterdam_value_create() {
        let interpreter = execute_amsterdam_value_create(ExecutionMode::Interpreter);
        let jit = execute_amsterdam_value_create(ExecutionMode::Jit);
        let aot = execute_amsterdam_value_create(ExecutionMode::Aot);

        assert_eq!(jit.output, interpreter.output);
        assert_eq!(aot.output, interpreter.output);
        assert_eq!(jit.state_root, interpreter.state_root);
        assert_eq!(aot.state_root, interpreter.state_root);
        assert_eq!(jit.logs_root, interpreter.logs_root);
        assert_eq!(aot.logs_root, interpreter.logs_root);
        assert_eq!(jit.gas_used, interpreter.gas_used);
        assert_eq!(aot.gas_used, interpreter.gas_used);
        assert_eq!(jit.evm_result, interpreter.evm_result);
        assert_eq!(aot.evm_result, interpreter.evm_result);
    }
}
