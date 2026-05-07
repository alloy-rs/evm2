use crate::{
    error::{TestError, TestErrorKind},
    types::{AccountInfo, Env, Test, TestSuite, TestUnit, TransactionParts, TxPartIndices},
};
use alloy_consensus::{TypedTransaction, transaction::Recovered};
use alloy_eips::{eip4844, eip7691, eip7702::SignedAuthorization};
use alloy_primitives::{Address, B256, Bytes, Log, TxKind, U256, keccak256};
use alloy_rpc_types_eth::{
    AccessList as RpcAccessList, AccessListItem as RpcAccessListItem, TransactionInput,
    TransactionRequest,
};
use alloy_trie::{
    TrieAccount,
    root::{state_root_unhashed, storage_root_unhashed},
};
use evm2::{
    BEACON_ROOTS_ADDRESS, BaseEvmTypes, Evm, EvmTypes, HISTORY_STORAGE_ADDRESS, Precompiles,
    SpecId, StorageKey, TxResult,
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
    for (spec_name, posts) in &unit.post {
        let Some(spec) = spec_name.to_spec_id() else {
            continue;
        };
        let block = parse_block(&unit.env, spec);
        for post in posts {
            let tx = match build_tx(&unit.transaction, &post.indexes, unit.env.current_chain_id) {
                Ok(tx) => tx,
                Err(_) if post.expect_exception.is_some() => {
                    continue;
                }
                Err(err) => return Err(TestError::case(path, name, err)),
            };
            let result = execute_spec(spec, block, state.clone(), &tx, &unit.env);
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
) -> Result<SpecOutcome, HandlerError> {
    macro_rules! run {
        ($spec:ident) => {{
            let spec = SpecId::$spec;
            let mut evm = Evm::<BaseEvmTypes<RecoveredTxEnvelope>>::new(
                spec,
                block,
                ethereum_tx_registry(),
                database.clone(),
                Precompiles::base(spec),
            );
            let system_changes = pre_block_system_calls(&mut evm, spec, env, &database);
            let result = evm.transact(tx)?;
            Ok(spec_outcome(&evm, result, &system_changes))
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
    system_changes: &[StateChanges],
) -> SpecOutcome {
    let mut post = evm.state().initial().clone();
    for changes in system_changes {
        post = apply_state_changes(&post, changes);
    }
    post = apply_state_changes(&post, &result.state_changes);

    SpecOutcome {
        state_root: state_root_from_database(&post),
        logs_root: logs_hash(&result.state_changes.logs),
        output: result.output,
        gas_used: result.gas_used,
        evm_result: format!("{:?}", result.stop),
    }
}

fn pre_block_system_calls<T: EvmTypes<Database = InMemoryDB, Host = Evm<T>>>(
    evm: &mut Evm<T>,
    spec: SpecId,
    env: &Env,
    database: &InMemoryDB,
) -> Vec<StateChanges> {
    if env.current_number.is_zero() {
        return Vec::new();
    }

    let mut changes = Vec::new();
    if spec.enables(SpecId::PRAGUE)
        && let Some(previous_hash) = env.previous_hash
        && system_contract_has_code(database, HISTORY_STORAGE_ADDRESS)
    {
        push_system_call_changes(
            evm,
            &mut changes,
            HISTORY_STORAGE_ADDRESS,
            Bytes::copy_from_slice(previous_hash.as_slice()),
        );
    }
    if spec.enables(SpecId::CANCUN)
        && let Some(beacon_root) = env.current_beacon_root
        && system_contract_has_code(database, BEACON_ROOTS_ADDRESS)
    {
        push_system_call_changes(
            evm,
            &mut changes,
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(beacon_root.as_slice()),
        );
    }
    changes
}

fn push_system_call_changes<T: EvmTypes<Database = InMemoryDB, Host = Evm<T>>>(
    evm: &mut Evm<T>,
    changes: &mut Vec<StateChanges>,
    address: Address,
    data: Bytes,
) {
    let result = evm.system_call(address, data);
    assert!(result.status, "pre-block system call failed: {address}");
    changes.push(result.state_changes);
}

fn system_contract_has_code(database: &InMemoryDB, address: Address) -> bool {
    database
        .cache
        .accounts
        .get(&address)
        .and_then(|info| database.cache.contracts.get(&info.code_hash))
        .is_some_and(|code| !code.is_empty())
}

fn logs_hash(logs: &[Log]) -> B256 {
    let mut out = Vec::with_capacity(alloy_rlp::list_length(logs));
    alloy_rlp::encode_list(logs, &mut out);
    keccak256(out)
}

fn state_root_from_database(state: &InMemoryDB) -> B256 {
    let accounts = state.cache.accounts.iter().map(|(&address, info)| {
        let storage = storage_for_root(state, address);
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
        post.cache.contracts.insert(code_hash, code.clone());
    }
    for (&address, storage) in &changes.storage {
        if storage.wipe {
            post.cache.storage.retain(|key, _| key.address() != address);
        }
        for (&key, change) in &storage.slots {
            if change.current.is_zero() {
                post.cache.storage.remove(&StorageKey::new(address, key));
            } else {
                post.cache.storage.insert(StorageKey::new(address, key), change.current);
            }
        }
    }
    for (&address, change) in &changes.accounts {
        match &change.current {
            Some(info) => post.insert_account_info(address, info.clone()),
            None => {
                post.cache.accounts.remove(&address);
                post.cache.storage.retain(|key, _| key.address() != address);
            }
        }
    }
    post
}

fn storage_for_root(state: &InMemoryDB, address: Address) -> Vec<(B256, U256)> {
    state
        .cache
        .storage
        .iter()
        .filter_map(|(&key, &value)| {
            (key.address() == address && !value.is_zero()).then_some((B256::from(key.key()), value))
        })
        .collect()
}

fn parse_state(pre: &BTreeMap<Address, AccountInfo>) -> Result<InMemoryDB, TestErrorKind> {
    let mut database = InMemoryDB::default();
    for (address, account) in pre {
        let mut info = EvmAccountInfo::default().with_code(parse_bytecode(account.code.clone()));
        info.nonce = account.nonce;
        info.balance = account.balance;
        database.insert_account_info(*address, info);
        for (key, value) in &account.storage {
            database.insert_account_storage(*address, *key, *value);
        }
    }
    Ok(database)
}

fn parse_bytecode(code: Bytes) -> Bytecode {
    Bytecode::new_raw_checked(code.clone()).unwrap_or_else(|_| Bytecode::new_legacy(code))
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

    let mut request = TransactionRequest::default()
        .from(caller)
        .gas_limit(gas_limit)
        .nonce(nonce)
        .value(value)
        .input(TransactionInput::from(data));
    request.to = Some(TxKind::from(raw.to));
    request.transaction_type = raw.tx_type;
    request.chain_id = chain_id
        .map(TryInto::try_into)
        .transpose()
        .map_err(|_| TestErrorKind::Overflow("chainId"))?;
    if !matches!(raw.tx_type, Some(2..=4)) {
        request.gas_price = raw
            .gas_price
            .map(TryInto::try_into)
            .transpose()
            .map_err(|_| TestErrorKind::Overflow("gasPrice"))?;
        if request.gas_price.is_none()
            && (matches!(raw.tx_type, Some(0 | 1))
                || (raw.max_fee_per_gas.is_none() && raw.max_priority_fee_per_gas.is_none()))
        {
            request.gas_price = Some(0);
        }
    }
    request.max_fee_per_gas = raw
        .max_fee_per_gas
        .map(TryInto::try_into)
        .transpose()
        .map_err(|_| TestErrorKind::Overflow("maxFeePerGas"))?;
    request.max_priority_fee_per_gas =
        if raw.max_fee_per_gas.is_some() && raw.max_priority_fee_per_gas.is_none() {
            Some(0)
        } else {
            raw.max_priority_fee_per_gas
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| TestErrorKind::Overflow("maxPriorityFeePerGas"))?
        };
    request.max_fee_per_blob_gas = raw
        .max_fee_per_blob_gas
        .map(TryInto::try_into)
        .transpose()
        .map_err(|_| TestErrorKind::Overflow("maxFeePerBlobGas"))?;
    request.access_list = access_list(raw, indexes.data)?;
    request.authorization_list = authorization_list(raw)?;
    if raw.max_fee_per_blob_gas.is_some()
        || matches!(raw.tx_type, Some(3))
        || !raw.blob_versioned_hashes.is_empty()
    {
        request.blob_versioned_hashes = Some(raw.blob_versioned_hashes.clone());
    }

    let tx =
        request.build_consensus_tx().map_err(|err| TestErrorKind::BuildTransaction(err.error))?;
    recovered_envelope(tx, caller)
}

fn authorization_list(
    raw: &TransactionParts,
) -> Result<Option<Vec<SignedAuthorization>>, TestErrorKind> {
    let Some(authorizations) = &raw.authorization_list else {
        return Ok(None);
    };
    let authorizations = authorizations
        .iter()
        .map(|authorization| serde_json::from_value(authorization.value.clone()))
        .collect::<Result<_, _>>()?;
    Ok(Some(authorizations))
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
    Ok(Some(RpcAccessList(
        access_list
            .into_iter()
            .map(|item| RpcAccessListItem {
                address: item.address,
                storage_keys: item.storage_keys,
            })
            .collect(),
    )))
}

fn recovered_envelope(
    tx: TypedTransaction,
    caller: Address,
) -> Result<RecoveredTxEnvelope, TestErrorKind> {
    match tx {
        TypedTransaction::Legacy(tx) => {
            Ok(RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, caller)))
        }
        TypedTransaction::Eip2930(tx) => {
            Ok(RecoveredTxEnvelope::Eip2930(Recovered::new_unchecked(tx, caller)))
        }
        TypedTransaction::Eip1559(tx) => {
            Ok(RecoveredTxEnvelope::Eip1559(Recovered::new_unchecked(tx, caller)))
        }
        TypedTransaction::Eip4844(tx) => {
            Ok(RecoveredTxEnvelope::Eip4844(Recovered::new_unchecked(tx, caller)))
        }
        TypedTransaction::Eip7702(tx) => {
            Ok(RecoveredTxEnvelope::Eip7702(Recovered::new_unchecked(tx, caller)))
        }
    }
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
            access_lists: vec![Some(vec![crate::types::AccessListItem {
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
                Some(vec![crate::types::AccessListItem {
                    address: first_address,
                    storage_keys: vec![B256::with_last_byte(1)],
                }]),
                Some(vec![crate::types::AccessListItem {
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
}
