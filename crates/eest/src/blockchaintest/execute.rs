use super::{
    error::{TestError, TestErrorKind},
    types::{
        Account, Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, Transaction,
        Withdrawal,
    },
};
use crate::{
    filter::EntryPoint,
    state::{insert_account_with_storage, parse_bytecode, system_contract_has_code},
    tx::{TxFields, build_recovered_tx, rpc_access_list, signed_authorizations},
};
use alloy_eips::eip7840::BlobParams;
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, U256};
use alloy_rpc_types_eth::AccessList as RpcAccessList;
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResult,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{
        AccountInfo as EvmAccountInfo, BEACON_ROOTS_ADDRESS, EmptyDB, HISTORY_STORAGE_ADDRESS,
        InMemoryDB, WITHDRAWAL_REQUEST_ADDRESS,
    },
    registry::HandlerError,
};
use std::{fs, mem, path::Path};

const ONE_GWEI: u64 = 1_000_000_000;
const ONE_ETHER: u128 = 1_000_000_000_000_000_000;

/// Execution options for a single suite.
#[derive(Clone, Copy, Debug)]
pub struct ExecuteConfig {
    /// Whether to validate final post-state when fixtures contain it.
    pub validate_post_state: bool,
}

impl Default for ExecuteConfig {
    fn default() -> Self {
        Self { validate_post_state: true }
    }
}

/// Per-file execution summary.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExecuteSummary {
    /// Number of executed test cases.
    pub executed: usize,
    /// Number of skipped test cases.
    pub skipped: usize,
}

/// Executes a single blockchain test JSON file using explicit execution options.
pub(crate) fn execute_test_suite(
    path: &Path,
    config: ExecuteConfig,
) -> Result<ExecuteSummary, TestError> {
    let input = fs::read_to_string(path).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_str_with_config(path, &input, config)
}

/// Executes a loaded blockchain test JSON file using explicit execution options.
pub fn execute_str_with_config(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
) -> Result<ExecuteSummary, TestError> {
    execute_str_with_filter(path, input, config, &EntryPoint::default())
}

/// Executes a loaded blockchain test JSON file, selecting cases by entrypoint.
pub fn execute_str_with_filter(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
    entrypoint: &EntryPoint,
) -> Result<ExecuteSummary, TestError> {
    let suite: BlockchainTest =
        serde_json::from_str(input).map_err(|err| TestError::unknown(path, err.into()))?;
    let mut summary = ExecuteSummary::default();
    for (name, unit) in suite.0 {
        if !entrypoint.matches(&name) {
            summary.skipped += 1;
            continue;
        }
        if unit.network.is_transition() {
            summary.skipped += 1;
            continue;
        }
        execute_case(path, &name, &unit, config)?;
        summary.executed += 1;
    }
    Ok(summary)
}

fn execute_case(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
    config: ExecuteConfig,
) -> Result<(), TestError> {
    let mut database =
        parse_state(&test_case.pre.0).map_err(|err| TestError::case(path, name, err))?;
    database.insert_block_hash(&U256::ZERO, &test_case.genesis_block_header.hash);

    let spec = fork_to_spec_id(test_case.network);
    let mut parent_block_hash = Some(test_case.genesis_block_header.hash);
    let mut parent_excess_blob_gas =
        test_case.genesis_block_header.excess_blob_gas.unwrap_or_default().saturating_to::<u64>();
    let mut block_env =
        block_env_from_header(&test_case.genesis_block_header, parent_excess_blob_gas, spec);

    for (block_index, block) in test_case.blocks.iter().enumerate() {
        execute_block(
            path,
            name,
            block_index,
            block,
            spec,
            &mut database,
            &mut block_env,
            &mut parent_block_hash,
            &mut parent_excess_blob_gas,
        )?;
    }

    if config.validate_post_state
        && let Some(expected) = &test_case.post_state
    {
        validate_post_state(&database, expected).map_err(|err| TestError::case(path, name, err))?;
    }

    Ok(())
}

#[expect(clippy::too_many_arguments)]
fn execute_block(
    path: &Path,
    name: &str,
    block_index: usize,
    block: &Block,
    spec: SpecId,
    database: &mut InMemoryDB,
    block_env: &mut BlockEnv,
    parent_block_hash: &mut Option<B256>,
    parent_excess_blob_gas: &mut u64,
) -> Result<(), TestError> {
    let should_fail = block.expect_exception.is_some();
    let mut block_hash = None;
    let mut beacon_root = None;
    let mut this_excess_blob_gas = None;

    let mut next_block_env = *block_env;
    if let Some(header) = block_header(block) {
        block_hash = Some(header.hash);
        beacon_root = header.parent_beacon_block_root;
        next_block_env = block_env_from_header(header, *parent_excess_blob_gas, spec);
        this_excess_blob_gas = header.excess_blob_gas.map(|gas| gas.saturating_to::<u64>());
    }

    let mut block_database = database.clone();
    pre_block_system_calls(
        &mut block_database,
        spec,
        next_block_env,
        *parent_block_hash,
        beacon_root,
    )
    .map_err(|err| TestError::case(path, name, err))?;

    for raw_tx in block_transactions(block) {
        let tx = match build_tx(raw_tx) {
            Ok(tx) => tx,
            Err(_err) if should_fail => {
                return Ok(());
            }
            Err(err) => return Err(TestError::case(path, name, err)),
        };

        match execute_tx(spec, next_block_env, &mut block_database, &tx) {
            Ok(_) => {}
            Err(err) if should_fail => {
                let _ = err;
                return Ok(());
            }
            Err(err) => return Err(TestError::case(path, name, err.into())),
        }
    }

    if should_fail {
        let expected = block.expect_exception.clone().unwrap_or_default();
        return Err(TestError::case(path, name, TestErrorKind::UnexpectedSuccess(expected)));
    }

    post_block_transition(&mut block_database, spec, next_block_env, block_withdrawals(block))
        .map_err(|err| TestError::case(path, name, err))?;

    if let Some(expected_bal) = &block.block_access_list {
        assert_block_access_list(block_index, expected_bal);
    }

    block_database.insert_block_hash(&next_block_env.number, &block_hash.unwrap_or_default());
    *database = block_database;
    *block_env = next_block_env;
    *parent_block_hash = block_hash;
    if let Some(excess_blob_gas) = this_excess_blob_gas {
        *parent_excess_blob_gas = excess_blob_gas;
    }
    Ok(())
}

fn block_header(block: &Block) -> Option<&BlockHeader> {
    block
        .block_header
        .as_ref()
        .or_else(|| block.rlp_decoded.as_ref().and_then(|decoded| decoded.block_header.as_ref()))
}

fn block_transactions(block: &Block) -> &[Transaction] {
    if let Some(transactions) = &block.transactions
        && !transactions.is_empty()
    {
        return transactions;
    }
    block.rlp_decoded.as_ref().map(|decoded| decoded.transactions.as_slice()).unwrap_or_default()
}

fn block_withdrawals(block: &Block) -> &[Withdrawal] {
    if let Some(withdrawals) = &block.withdrawals
        && !withdrawals.is_empty()
    {
        return withdrawals;
    }
    block.rlp_decoded.as_ref().map(|decoded| decoded.withdrawals.as_slice()).unwrap_or_default()
}

fn pre_block_system_calls(
    database: &mut InMemoryDB,
    spec: SpecId,
    block: BlockEnv,
    parent_block_hash: Option<B256>,
    parent_beacon_block_root: Option<B256>,
) -> Result<(), TestErrorKind> {
    if block.number.is_zero() {
        return Ok(());
    }
    if spec.enables(SpecId::PRAGUE)
        && let Some(hash) = parent_block_hash
    {
        run_system_call(
            database,
            spec,
            block,
            HISTORY_STORAGE_ADDRESS,
            Bytes::copy_from_slice(hash.as_slice()),
            "eip2935",
        )?;
    }
    if spec.enables(SpecId::CANCUN)
        && let Some(root) = parent_beacon_block_root
    {
        run_system_call(
            database,
            spec,
            block,
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(root.as_slice()),
            "eip4788",
        )?;
    }
    Ok(())
}

fn post_block_transition(
    database: &mut InMemoryDB,
    spec: SpecId,
    block: BlockEnv,
    withdrawals: &[Withdrawal],
) -> Result<(), TestErrorKind> {
    let reward = block_reward(spec, 0);
    if reward != 0 {
        increment_balance(database, block.beneficiary, U256::from(reward));
    }

    if spec.enables(SpecId::SHANGHAI) {
        for withdrawal in withdrawals {
            increment_balance(
                database,
                withdrawal.address,
                withdrawal.amount.saturating_mul(U256::from(ONE_GWEI)),
            );
        }
    }

    if spec.enables(SpecId::PRAGUE) {
        run_system_call(
            database,
            spec,
            block,
            WITHDRAWAL_REQUEST_ADDRESS,
            Bytes::new(),
            "eip7002",
        )?;
        run_system_call(
            database,
            spec,
            block,
            evm2::evm::CONSOLIDATION_REQUEST_ADDRESS,
            Bytes::new(),
            "eip7251",
        )?;
    }
    Ok(())
}

fn run_system_call(
    database: &mut InMemoryDB,
    spec: SpecId,
    block: BlockEnv,
    address: Address,
    data: Bytes,
    label: &'static str,
) -> Result<(), TestErrorKind> {
    let mut evm = Evm::<BaseEvmTypes>::new(
        spec,
        block,
        ethereum_tx_registry(spec),
        mem::take(database),
        Precompiles::base(spec),
    );
    let result = evm.system_call(address, data);
    evm.merge_cache::<EmptyDB>();
    *database = mem::take(evm.database_as_mut::<InMemoryDB>().expect("database type mismatch"));
    if !result.status && system_contract_has_code(database, address) {
        return Err(TestErrorKind::SystemCall(label));
    }
    Ok(())
}

fn execute_tx(
    spec: SpecId,
    block: BlockEnv,
    database: &mut InMemoryDB,
    tx: &RecoveredTxEnvelope,
) -> Result<TxResult, HandlerError> {
    let mut evm = Evm::<BaseEvmTypes>::new(
        spec,
        block,
        ethereum_tx_registry(spec),
        mem::take(database),
        Precompiles::base(spec),
    );
    let result = evm.transact(tx);
    evm.merge_cache::<EmptyDB>();
    *database = mem::take(evm.database_as_mut::<InMemoryDB>().expect("database type mismatch"));
    result
}

fn parse_state(
    accounts: &std::collections::BTreeMap<Address, Account>,
) -> Result<InMemoryDB, TestErrorKind> {
    let mut database = InMemoryDB::default();
    for (address, account) in accounts {
        let nonce = account.nonce.try_into().map_err(|_| TestErrorKind::Overflow("nonce"))?;
        let info = EvmAccountInfo::default()
            .with_balance(account.balance)
            .with_nonce(nonce)
            .with_code(parse_bytecode(account.code.clone()));
        insert_account_with_storage(
            &mut database,
            *address,
            info,
            account.storage.iter().map(|(&key, &value)| (key, value)),
        );
    }
    Ok(database)
}

fn block_env_from_header(header: &BlockHeader, excess_blob_gas: u64, spec: SpecId) -> BlockEnv {
    let excess_blob_gas =
        header.excess_blob_gas.map(|gas| gas.saturating_to::<u64>()).unwrap_or(excess_blob_gas);
    BlockEnv {
        number: header.number,
        beneficiary: header.coinbase,
        timestamp: header.timestamp,
        gas_limit: header.gas_limit,
        basefee: header.base_fee_per_gas.unwrap_or_default(),
        difficulty: header.difficulty,
        prevrandao: if header.difficulty.is_zero() {
            U256::from_be_slice(header.mix_hash.as_slice())
        } else {
            U256::ZERO
        },
        blob_basefee: U256::from(
            blob_params_for_timestamp(header.timestamp, spec).calc_blob_fee(excess_blob_gas),
        ),
        slot_num: header.slot_number.unwrap_or_default(),
        ext: (),
        _non_exhaustive: (),
    }
}

fn blob_params_for_timestamp(timestamp: U256, spec: SpecId) -> BlobParams {
    const MAINNET_BPO1_TIMESTAMP: u64 = 1_765_290_071;
    const MAINNET_BPO2_TIMESTAMP: u64 = 1_767_747_671;

    if timestamp.to::<u64>() >= MAINNET_BPO2_TIMESTAMP {
        BlobParams::bpo2()
    } else if timestamp.to::<u64>() >= MAINNET_BPO1_TIMESTAMP {
        BlobParams::bpo1()
    } else if spec.enables(SpecId::OSAKA) {
        BlobParams::osaka()
    } else if spec.enables(SpecId::PRAGUE) {
        BlobParams::prague()
    } else {
        BlobParams::cancun()
    }
}

fn build_tx(raw: &Transaction) -> Result<RecoveredTxEnvelope, TestErrorKind> {
    let caller = raw.sender.ok_or(TestErrorKind::MissingSender)?;
    let gas_limit = raw.gas_limit.try_into().map_err(|_| TestErrorKind::Overflow("gasLimit"))?;
    let nonce = raw.nonce.try_into().map_err(|_| TestErrorKind::Overflow("nonce"))?;

    Ok(build_recovered_tx(TxFields {
        tx_type: Some(raw.tx_type()),
        caller,
        kind: raw.kind(),
        data: raw.data.clone(),
        gas_limit,
        nonce,
        value: raw.value,
        chain_id: raw.chain_id,
        gas_price: raw.gas_price,
        max_fee_per_gas: raw.max_fee_per_gas,
        max_priority_fee_per_gas: raw.max_priority_fee_per_gas,
        access_list: access_list(raw)?,
        authorization_list: signed_authorizations(raw.authorization_list.as_deref())?,
        blob_versioned_hashes: raw.blob_versioned_hashes.clone(),
        max_fee_per_blob_gas: raw.max_fee_per_blob_gas,
    })?)
}

fn access_list(raw: &Transaction) -> Result<Option<RpcAccessList>, TestErrorKind> {
    if matches!(raw.tx_type(), 0) {
        return Ok(None);
    }
    let Some(access_list) = &raw.access_list else {
        return Ok(matches!(raw.tx_type(), 1).then(RpcAccessList::default));
    };
    Ok(Some(rpc_access_list(access_list.iter())))
}

const fn block_reward(spec: SpecId, ommers: usize) -> u128 {
    if spec.enables(SpecId::MERGE) {
        return 0;
    }
    let reward = if spec.enables(SpecId::PETERSBURG) {
        ONE_ETHER * 2
    } else if spec.enables(SpecId::BYZANTIUM) {
        ONE_ETHER * 3
    } else {
        ONE_ETHER * 5
    };
    reward + (reward >> 5) * ommers as u128
}

fn increment_balance(database: &mut InMemoryDB, address: Address, amount: U256) {
    let mut info = database.cache.accounts.get(&address).cloned().flatten().unwrap_or_default();
    info.balance = info.balance.saturating_add(amount);
    if info.code_hash.is_zero() {
        info.code_hash = KECCAK256_EMPTY;
    }
    database.cache.accounts.insert(address, Some(info));
}

fn validate_post_state(
    database: &InMemoryDB,
    expected: &std::collections::BTreeMap<Address, Account>,
) -> Result<(), TestErrorKind> {
    for (address, expected_account) in expected {
        let info = database.cache.accounts.get(address).cloned().flatten().unwrap_or_default();
        if info.balance != expected_account.balance {
            return Err(TestErrorKind::UnexpectedFailure(format!(
                "balance mismatch for {address}: got {}, expected {}",
                info.balance, expected_account.balance
            )));
        }
        if info.nonce != expected_account.nonce.saturating_to::<u64>() {
            return Err(TestErrorKind::UnexpectedFailure(format!(
                "nonce mismatch for {address}: got {}, expected {}",
                info.nonce, expected_account.nonce
            )));
        }

        if !expected_account.code.is_empty() {
            let actual_code = info
                .code
                .as_ref()
                .or_else(|| database.cache.contracts.get(&info.code_hash))
                .map(|code| code.original_byte_slice())
                .unwrap_or_default();
            if actual_code != expected_account.code.as_ref() {
                return Err(TestErrorKind::UnexpectedFailure(format!(
                    "code mismatch for {address}: got 0x{}, expected 0x{}",
                    alloy_primitives::hex::encode(actual_code),
                    alloy_primitives::hex::encode(&expected_account.code)
                )));
            }
        }

        for (&key, &value) in &database.cache.storage {
            if key.address() == *address
                && !value.is_zero()
                && !expected_account.storage.contains_key(&key.key())
            {
                return Err(TestErrorKind::UnexpectedFailure(format!(
                    "unexpected storage for {address}[{}]: got {}, expected 0",
                    key.key(),
                    value
                )));
            }
        }

        for (&slot, &expected_value) in &expected_account.storage {
            let actual_value = database
                .cache
                .storage
                .get(&evm2::StorageKey::new(*address, slot))
                .copied()
                .unwrap_or_default();
            if actual_value != expected_value {
                return Err(TestErrorKind::UnexpectedFailure(format!(
                    "storage mismatch for {address}[{slot}]: got {actual_value}, expected {expected_value}"
                )));
            }
        }
    }
    Ok(())
}

fn assert_block_access_list(_block_index: usize, _expected: &alloy_eip7928::BlockAccessList) {
    todo!("evm2 does not build block access lists yet")
}

fn fork_to_spec_id(fork: ForkSpec) -> SpecId {
    match fork {
        ForkSpec::Frontier => SpecId::FRONTIER,
        ForkSpec::Homestead => SpecId::HOMESTEAD,
        ForkSpec::EIP150 => SpecId::TANGERINE,
        ForkSpec::EIP158 => SpecId::SPURIOUS_DRAGON,
        ForkSpec::Byzantium => SpecId::BYZANTIUM,
        ForkSpec::Constantinople | ForkSpec::ConstantinopleFix => SpecId::PETERSBURG,
        ForkSpec::Istanbul => SpecId::ISTANBUL,
        ForkSpec::Berlin => SpecId::BERLIN,
        ForkSpec::London => SpecId::LONDON,
        ForkSpec::Paris
        | ForkSpec::MergeEOF
        | ForkSpec::MergeMeterInitCode
        | ForkSpec::MergePush0 => SpecId::MERGE,
        ForkSpec::Shanghai => SpecId::SHANGHAI,
        ForkSpec::Cancun => SpecId::CANCUN,
        ForkSpec::Prague => SpecId::PRAGUE,
        ForkSpec::Osaka => SpecId::OSAKA,
        ForkSpec::Amsterdam => SpecId::AMSTERDAM,
        ForkSpec::FrontierToHomesteadAt5
        | ForkSpec::HomesteadToDaoAt5
        | ForkSpec::HomesteadToEIP150At5
        | ForkSpec::EIP158ToByzantiumAt5
        | ForkSpec::ByzantiumToConstantinopleAt5
        | ForkSpec::ByzantiumToConstantinopleFixAt5
        | ForkSpec::BerlinToLondonAt5
        | ForkSpec::ParisToShanghaiAtTime15k
        | ForkSpec::ShanghaiToCancunAtTime15k
        | ForkSpec::CancunToPragueAtTime15k
        | ForkSpec::PragueToOsakaAtTime15k
        | ForkSpec::OsakaToBPO1AtTime15k
        | ForkSpec::BPO1ToBPO2AtTime15k
        | ForkSpec::BPO2ToBPO3AtTime15k
        | ForkSpec::BPO3ToBPO4AtTime15k
        | ForkSpec::BPO2ToAmsterdamAtTime15k => unreachable!("transition forks are skipped"),
    }
}
