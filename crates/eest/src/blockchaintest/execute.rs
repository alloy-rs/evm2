use super::{
    error::{TestError, TestErrorKind},
    hook::{
        BlockFailed, BlockFinished, BlockStarted, CaseStarted, Hook, NoopHook, TransactionFailed,
        TransactionFinished, TransactionStarted,
    },
    types::{
        Account, Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, Transaction,
        Withdrawal,
    },
};
#[cfg(feature = "jit")]
use crate::compiled::{self, FileSummary};
use crate::{
    execution::ExecutionResources,
    filter::EntryPoint,
    fixture_io,
    forks::is_fork_skipped,
    state::{insert_account_with_storage, parse_bytecode},
    tx::{TxFields, build_recovered_tx, rpc_access_list, signed_authorizations},
};
use alloy_eips::eip7840::BlobParams;
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, U256};
use alloy_rpc_types_eth::AccessList as RpcAccessList;
use anstyle::{AnsiColor, Color, Style};
use evm2::{
    BaseEvmTypes, ErrorCode, Evm, Precompiles, SpecId, TxResult,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{
        AccountChangeRef, AccountInfo as EvmAccountInfo, AccountInfoRef, BEACON_ROOTS_ADDRESS,
        BlockStateAccumulator, DbStats, DbStatsCounts, HISTORY_STORAGE_ADDRESS, InMemoryDB,
        StateChangeSink, StateChangeSource, SystemTx, Tee, WITHDRAWAL_REQUEST_ADDRESS,
    },
    registry::HandlerError,
};
#[cfg(feature = "jit")]
use std::path::PathBuf;
use std::{mem, path::Path};

pub use crate::execution::ExecutionMode;

const ONE_GWEI: u64 = 1_000_000_000;
const ONE_ETHER: u128 = 1_000_000_000_000_000_000;

/// Execution options for a single suite.
#[derive(Clone, Copy, Debug)]
pub struct ExecuteConfig {
    /// Whether to validate final post-state when fixtures contain it.
    pub validate_post_state: bool,
    /// Execution backend.
    pub mode: ExecutionMode,
    /// Whether to print database method call counts.
    pub db_stats: bool,
}

impl Default for ExecuteConfig {
    fn default() -> Self {
        Self { validate_post_state: true, mode: ExecutionMode::Interpreter, db_stats: false }
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

/// Executes a single blockchain test file using explicit execution options.
pub(crate) fn execute_test_suite(
    path: &Path,
    config: ExecuteConfig,
) -> Result<ExecuteSummary, TestError> {
    let suite =
        fixture_io::read_blockchain(path).map_err(|err| TestError::unknown(path, err.into()))?;
    let entrypoint = EntryPoint::default();
    let mut hook = NoopHook;
    execute_suite(path, &suite, config, &entrypoint, &mut hook)
}

/// Executes multiple blockchain test JSON files using one shared execution resource set.
#[cfg(feature = "jit")]
pub(crate) fn execute_test_suites(
    paths: &[PathBuf],
    config: ExecuteConfig,
) -> Result<ExecuteSummary, TestError> {
    let error_path = paths.first().map_or_else(|| Path::new("blockchain tests"), PathBuf::as_path);
    let resources = ExecutionResources::new(config.mode)
        .map_err(|err| TestError::unknown(error_path, err.into()))?;
    let summary = compiled::run_files(paths.to_vec(), resources, move |path, resources| {
        let suite = fixture_io::read_blockchain(&path)
            .map_err(|err| TestError::unknown(path.as_path(), err.into()))?;
        let entrypoint = EntryPoint::default();
        let mut hook = NoopHook;
        let file_summary = execute_suite_with_resources(
            &path,
            &suite,
            config,
            &entrypoint,
            &mut hook,
            &resources,
        )?;
        Ok(FileSummary {
            executed: file_summary.executed,
            skipped: file_summary.skipped,
            db_stats_counts: DbStatsCounts::default(),
        })
    })?;

    Ok(ExecuteSummary { executed: summary.executed, skipped: summary.skipped })
}

/// Executes a loaded blockchain test JSON file.
pub fn execute_str(
    path: &Path,
    input: &str,
    config: ExecuteConfig,
    entrypoint: &EntryPoint,
    hook: &mut dyn Hook,
) -> Result<ExecuteSummary, TestError> {
    let suite: BlockchainTest =
        serde_json::from_str(input).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_suite(path, &suite, config, entrypoint, hook)
}

/// Executes a parsed blockchain test suite.
pub fn execute_suite(
    path: &Path,
    suite: &BlockchainTest,
    config: ExecuteConfig,
    entrypoint: &EntryPoint,
    hook: &mut dyn Hook,
) -> Result<ExecuteSummary, TestError> {
    let resources =
        ExecutionResources::new(config.mode).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_suite_with_resources(path, suite, config, entrypoint, hook, &resources)
}

fn execute_suite_with_resources(
    path: &Path,
    suite: &BlockchainTest,
    config: ExecuteConfig,
    entrypoint: &EntryPoint,
    hook: &mut dyn Hook,
    resources: &ExecutionResources,
) -> Result<ExecuteSummary, TestError> {
    let mut summary = ExecuteSummary::default();
    for (name, test_case) in &suite.0 {
        if !entrypoint.matches(name)
            || test_case.network.is_transition()
            || is_blockchain_fork_skipped(fork_to_spec_id(test_case.network))
        {
            summary.skipped += 1;
            continue;
        }
        execute_case(path, name, test_case, config, hook, resources)?;
        summary.executed += 1;
    }
    Ok(summary)
}

fn execute_case(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
    config: ExecuteConfig,
    hook: &mut dyn Hook,
    resources: &ExecutionResources,
) -> Result<(), TestError> {
    let mut database =
        parse_state(&test_case.pre.0).map_err(|err| TestError::case(path, name, err))?;
    seed_block_hashes(&mut database, test_case);

    let spec = fork_to_spec_id(test_case.network);
    let mut parent_block_hash = Some(test_case.genesis_block_header.hash);
    let mut parent_excess_blob_gas =
        test_case.genesis_block_header.excess_blob_gas.unwrap_or_default().saturating_to::<u64>();
    let mut block_env =
        block_env_from_header(&test_case.genesis_block_header, parent_excess_blob_gas, spec);
    let total_blocks = test_case.blocks.len();
    let mut db_stats_counts = DbStatsCounts::default();

    hook.case_started(CaseStarted { name, total_blocks, network: test_case.network });

    for (block_index, block) in test_case.blocks.iter().enumerate() {
        let block_number = block_number(block);
        let block_gas_used = block_gas_used(block);
        let total_transactions = block_transactions(block).len();
        hook.block_started(BlockStarted {
            block_index,
            total_blocks,
            block_number,
            block_gas_used,
            total_transactions,
        });
        match execute_block(
            path,
            name,
            block_index,
            total_blocks,
            block_number,
            total_transactions,
            block,
            spec,
            &mut database,
            &mut block_env,
            &mut parent_block_hash,
            &mut parent_excess_blob_gas,
            hook,
            resources,
            if config.db_stats { Some(&mut db_stats_counts) } else { None },
        ) {
            Ok(()) => hook.block_finished(BlockFinished {
                block_index,
                total_blocks,
                block_number,
                block_gas_used,
            }),
            Err(err) => {
                hook.block_failed(BlockFailed {
                    block_index,
                    total_blocks,
                    block_number,
                    block_gas_used,
                    error: &err,
                });
                return Err(err);
            }
        }
    }

    if config.validate_post_state
        && let Some(expected) = &test_case.post_state
    {
        validate_post_state(&database, expected).map_err(|err| TestError::case(path, name, err))?;
    }
    if config.db_stats {
        print_db_stats(db_stats_counts);
    }
    Ok(())
}

fn seed_block_hashes(database: &mut InMemoryDB, test_case: &BlockchainTestCase) {
    for block_hash in &test_case.block_hashes {
        database.insert_block_hash(&block_hash.number, &block_hash.hash);
    }
    database.insert_block_hash(
        &test_case.genesis_block_header.number,
        &test_case.genesis_block_header.hash,
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockResolution {
    Commit,
    Discard,
}

#[expect(clippy::too_many_arguments)]
fn execute_block(
    path: &Path,
    name: &str,
    block_index: usize,
    total_blocks: usize,
    block_number: Option<U256>,
    total_transactions: usize,
    block: &Block,
    spec: SpecId,
    database: &mut InMemoryDB,
    block_env: &mut BlockEnv,
    parent_block_hash: &mut Option<B256>,
    parent_excess_blob_gas: &mut u64,
    hook: &mut dyn Hook,
    resources: &ExecutionResources,
    db_stats_counts: Option<&mut DbStatsCounts>,
) -> Result<(), TestError> {
    let db_stats = db_stats_counts.is_some();
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

    let initial_database = mem::take(database);
    let mut evm = if db_stats {
        Evm::<BaseEvmTypes>::new(
            spec,
            next_block_env,
            ethereum_tx_registry(spec),
            DbStats::new(initial_database),
            Precompiles::base(spec),
        )
    } else {
        Evm::<BaseEvmTypes>::new(
            spec,
            next_block_env,
            ethereum_tx_registry(spec),
            initial_database,
            Precompiles::base(spec),
        )
    };
    resources.configure_evm(&mut evm);
    let mut block_state = BlockStateAccumulator::new();

    let result = (|| -> Result<BlockResolution, TestError> {
        pre_block_system_calls(
            &mut evm,
            &mut block_state,
            spec,
            next_block_env,
            *parent_block_hash,
            beacon_root,
        )
        .map_err(|err| TestError::case(path, name, err))?;

        let transactions = block_transactions(block);
        // EIP-8037: track regular and state gas separately for the block-header gas check.
        let mut cumulative_tx_gas_used = 0u64;
        let mut block_regular_gas_used = 0u64;
        let mut block_state_gas_used = 0u64;
        for (transaction_index, raw_tx) in transactions.iter().enumerate() {
            hook.transaction_started(TransactionStarted {
                block_index,
                total_blocks,
                block_number,
                transaction_index,
                total_transactions,
            });
            let tx = match build_tx(raw_tx) {
                Ok(tx) => tx,
                Err(_err) if should_fail => {
                    return Ok(BlockResolution::Discard);
                }
                Err(err) => {
                    hook.transaction_failed(TransactionFailed {
                        block_index,
                        total_blocks,
                        block_number,
                        transaction_index,
                        total_transactions,
                        error: &err,
                    });
                    return Err(TestError::case(path, name, err));
                }
            };

            match execute_tx(&mut evm, &mut block_state, &tx) {
                Ok(result) => {
                    cumulative_tx_gas_used =
                        cumulative_tx_gas_used.saturating_add(result.tx_gas_used());
                    block_regular_gas_used =
                        block_regular_gas_used.saturating_add(result.regular_gas_spent());
                    block_state_gas_used =
                        block_state_gas_used.saturating_add(result.state_gas_spent());
                    hook.transaction_finished(TransactionFinished {
                        block_index,
                        total_blocks,
                        block_number,
                        transaction_index,
                        total_transactions,
                    });
                }
                Err(err) if should_fail => {
                    let _ = err;
                    return Ok(BlockResolution::Discard);
                }
                Err(err) => {
                    hook.transaction_failed(TransactionFailed {
                        block_index,
                        total_blocks,
                        block_number,
                        transaction_index,
                        total_transactions,
                        error: &err,
                    });
                    return Err(TestError::case(path, name, err.into()));
                }
            }
        }

        if should_fail {
            let expected = block.expect_exception.clone().unwrap_or_default();
            return Err(TestError::case(path, name, TestErrorKind::UnexpectedSuccess(expected)));
        }

        // Validate the block header's gas used against the executed transactions. Under EIP-8037
        // (Amsterdam+) regular and state gas are tracked separately and the header records their
        // max; earlier forks record the cumulative per-transaction gas used (refunds included).
        if let Some(expected) = block_gas_used(block) {
            let expected = expected.saturating_to::<u64>();
            let actual = if spec.enables(SpecId::AMSTERDAM) {
                block_regular_gas_used.max(block_state_gas_used)
            } else {
                cumulative_tx_gas_used
            };
            if actual != expected {
                return Err(TestError::case(
                    path,
                    name,
                    TestErrorKind::BlockGasUsedMismatch { expected, actual },
                ));
            }
        }

        post_block_transition(
            &mut evm,
            &mut block_state,
            spec,
            next_block_env,
            block_withdrawals(block),
        )
        .map_err(|err| TestError::case(path, name, err))?;

        if let Some(expected_bal) = &block.block_access_list {
            assert_block_access_list(block_index, expected_bal);
        }

        Ok(BlockResolution::Commit)
    })();

    // The EVM was constructed with this concrete database above; recover it before returning so
    // invalid blocks leave the caller's state unchanged.
    let (mut restored_database, block_db_stats_counts) = if db_stats {
        let stats = evm
            .database_as_mut::<DbStats<InMemoryDB>>()
            .expect("block EVM database should be DbStats<InMemoryDB>");
        (mem::take(stats.inner_mut()), Some(stats.counts()))
    } else {
        (
            mem::take(
                evm.database_as_mut::<InMemoryDB>()
                    .expect("block EVM database should be InMemoryDB"),
            ),
            None,
        )
    };
    if let Some(counts) = db_stats_counts
        && let Some(block_counts) = block_db_stats_counts
    {
        *counts += block_counts;
    }

    match result {
        Ok(BlockResolution::Commit) => {
            restored_database.commit_source(&block_state);
            restored_database
                .insert_block_hash(&next_block_env.number, &block_hash.unwrap_or_default());
            *database = restored_database;
            *block_env = next_block_env;
            *parent_block_hash = block_hash;
            if let Some(excess_blob_gas) = this_excess_blob_gas {
                *parent_excess_blob_gas = excess_blob_gas;
            }
            Ok(())
        }
        Ok(BlockResolution::Discard) => {
            *database = restored_database;
            Ok(())
        }
        Err(err) => {
            *database = restored_database;
            Err(err)
        }
    }
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

fn block_number(block: &Block) -> Option<U256> {
    block_header(block).map(|header| header.number)
}

fn block_gas_used(block: &Block) -> Option<U256> {
    block_header(block).map(|header| header.gas_used)
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
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
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
            evm,
            block_state,
            HISTORY_STORAGE_ADDRESS,
            Bytes::copy_from_slice(hash.as_slice()),
            "eip2935",
        )?;
    }
    if spec.enables(SpecId::CANCUN)
        && let Some(root) = parent_beacon_block_root
    {
        run_system_call(
            evm,
            block_state,
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(root.as_slice()),
            "eip4788",
        )?;
    }
    Ok(())
}

fn post_block_transition(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    spec: SpecId,
    block: BlockEnv,
    withdrawals: &[Withdrawal],
) -> Result<(), TestErrorKind> {
    let reward = block_reward(spec, 0);
    if reward != 0 {
        increment_balance(evm, block_state, block.beneficiary, U256::from(reward))?;
    }

    if spec.enables(SpecId::SHANGHAI) {
        for withdrawal in withdrawals {
            increment_balance(
                evm,
                block_state,
                withdrawal.address,
                withdrawal.amount.saturating_mul(U256::from(ONE_GWEI)),
            )?;
        }
    }

    if spec.enables(SpecId::PRAGUE) {
        run_system_call(evm, block_state, WITHDRAWAL_REQUEST_ADDRESS, Bytes::new(), "eip7002")?;
        run_system_call(
            evm,
            block_state,
            evm2::evm::CONSOLIDATION_REQUEST_ADDRESS,
            Bytes::new(),
            "eip7251",
        )?;
    }
    Ok(())
}

fn run_system_call(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    address: Address,
    data: Bytes,
    label: &'static str,
) -> Result<(), TestErrorKind> {
    let executed = evm.system_call(SystemTx::new(address, data))?;
    if !executed.result().status {
        let _ = executed.discard();
        let has_code = match evm.account_code(&address) {
            Ok(code) => !code.is_empty(),
            Err(code) => return Err(database_error(evm, code)),
        };
        if has_code {
            return Err(TestErrorKind::SystemCall(label));
        }
        return Ok(());
    }
    let _ = executed.commit_to(block_state);
    Ok(())
}

fn execute_tx(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    tx: &RecoveredTxEnvelope,
) -> Result<TxResult, HandlerError> {
    Ok(evm.transact(tx)?.commit_to(block_state))
}

fn commit_state_changes<S: StateChangeSource>(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    changes: &S,
) {
    let mut sink = Tee::new(evm.overlay_db_mut(), block_state);
    let Ok(()) = changes.visit(&mut sink);
}

struct AccountStateChange {
    address: Address,
    original: Option<EvmAccountInfo>,
    current: Option<EvmAccountInfo>,
}

impl StateChangeSource for AccountStateChange {
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        sink.account(AccountChangeRef {
            address: self.address,
            original: self.original.as_ref().map(account_info_ref),
            current: self.current.as_ref().map(account_info_ref),
        })
    }
}

const fn account_info_ref(info: &EvmAccountInfo) -> AccountInfoRef<'_> {
    AccountInfoRef {
        balance: info.balance,
        nonce: info.nonce,
        code_hash: info.code_hash,
        code: info.code.as_ref(),
    }
}

fn database_error(evm: &mut Evm<'_, BaseEvmTypes>, code: ErrorCode) -> TestErrorKind {
    TestErrorKind::UnexpectedFailure(evm.database_mut().error(code).to_string())
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

fn increment_balance(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    address: Address,
    amount: U256,
) -> Result<(), TestErrorKind> {
    let original = match evm.read_account_info(&address) {
        Ok(info) => info,
        Err(code) => return Err(database_error(evm, code)),
    };
    let mut current = original.clone().unwrap_or_default();
    current.balance = current.balance.saturating_add(amount);
    if current.code_hash.is_zero() {
        current.code_hash = KECCAK256_EMPTY;
    }

    let change = AccountStateChange { address, original, current: Some(current) };
    commit_state_changes(evm, block_state, &change);
    Ok(())
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

        if let Some(storage) = database.cache.storage.get(address) {
            for (&key, &value) in &storage.slots {
                if !value.is_zero() && !expected_account.storage.contains_key(&key) {
                    return Err(TestErrorKind::UnexpectedFailure(format!(
                        "unexpected storage for {address}[{key}]: got {value}, expected 0"
                    )));
                }
            }
        }

        for (&slot, &expected_value) in &expected_account.storage {
            let actual_value = database
                .cache
                .storage
                .get(address)
                .and_then(|storage| storage.slots.get(&slot))
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

/// Whether a blockchain test targeting `spec` should be skipped.
///
/// In addition to the globally unsupported forks ([`is_fork_skipped`]), Amsterdam and later are
/// skipped at the blockchain layer only. Amsterdam blocks require building and validating block
/// access lists ([EIP-7928], see the `todo!` in [`assert_block_access_list`]) and block-level gas
/// accounting without refunds (EIP-7778), neither of which evm2 implements yet, so every Amsterdam
/// blockchain fixture fails on block structure/gas regardless of the EIP it targets. State tests
/// have no block layer, so [`crate::runner`] still runs the Amsterdam state suite.
///
/// [EIP-7928]: https://eips.ethereum.org/EIPS/eip-7928
fn is_blockchain_fork_skipped(spec: SpecId) -> bool {
    is_fork_skipped(spec) || spec.enables(SpecId::AMSTERDAM)
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "jit")]
    use super::super::types::{SealEngine, State};
    #[cfg(feature = "jit")]
    use super::*;
    #[cfg(feature = "jit")]
    use evm2::interpreter::op;
    #[cfg(feature = "jit")]
    use std::collections::BTreeMap;

    #[cfg(feature = "jit")]
    const BYTECODE_STORE42: &[u8] = &[op::PUSH1, 0x42, op::PUSH0, op::SSTORE, op::STOP];

    #[cfg(feature = "jit")]
    fn execute_simple_storage_block(mode: ExecutionMode) -> ExecuteSummary {
        let caller = Address::from([0x11; 20]);
        let target = Address::from([0x22; 20]);
        let genesis_hash = B256::with_last_byte(1);
        let block_hash = B256::with_last_byte(2);
        let mut pre = BTreeMap::new();
        pre.insert(
            caller,
            Account {
                balance: U256::from(1_000_000_000),
                code: Bytes::new(),
                nonce: U256::ZERO,
                storage: BTreeMap::new(),
            },
        );
        pre.insert(
            target,
            Account {
                balance: U256::ZERO,
                code: Bytes::copy_from_slice(BYTECODE_STORE42),
                nonce: U256::ZERO,
                storage: BTreeMap::new(),
            },
        );

        let mut target_storage = BTreeMap::new();
        target_storage.insert(U256::ZERO, U256::from(0x42));
        let mut post_state = BTreeMap::new();
        post_state.insert(
            caller,
            Account {
                balance: U256::from(1_000_000_000),
                code: Bytes::new(),
                nonce: U256::ONE,
                storage: BTreeMap::new(),
            },
        );
        post_state.insert(
            target,
            Account {
                balance: U256::ZERO,
                code: Bytes::copy_from_slice(BYTECODE_STORE42),
                nonce: U256::ZERO,
                storage: target_storage,
            },
        );

        let suite = BlockchainTest(BTreeMap::from([(
            "simple-storage".to_string(),
            BlockchainTestCase {
                genesis_block_header: BlockHeader {
                    hash: genesis_hash,
                    number: U256::ZERO,
                    gas_limit: U256::from(30_000_000),
                    base_fee_per_gas: Some(U256::ZERO),
                    ..BlockHeader::default()
                },
                genesis_rlp: None,
                blocks: vec![Block {
                    block_header: Some(BlockHeader {
                        parent_hash: genesis_hash,
                        hash: block_hash,
                        number: U256::ONE,
                        gas_limit: U256::from(30_000_000),
                        gas_used: U256::from(43_105),
                        base_fee_per_gas: Some(U256::ZERO),
                        timestamp: U256::ONE,
                        ..BlockHeader::default()
                    }),
                    transactions: Some(vec![Transaction {
                        transaction_type: None,
                        sender: Some(caller),
                        data: Bytes::new(),
                        gas_limit: U256::from(100_000),
                        gas_price: Some(U256::ZERO),
                        nonce: U256::ZERO,
                        r: U256::ZERO,
                        s: U256::ZERO,
                        v: U256::ZERO,
                        value: U256::ZERO,
                        to: Some(target),
                        chain_id: Some(U256::ONE),
                        access_list: None,
                        max_fee_per_gas: None,
                        max_priority_fee_per_gas: None,
                        blob_versioned_hashes: Vec::new(),
                        max_fee_per_blob_gas: None,
                        authorization_list: None,
                        hash: None,
                    }]),
                    ..Block::default()
                }],
                post_state: Some(post_state),
                pre: State(pre),
                block_hashes: Vec::new(),
                lastblockhash: block_hash,
                network: ForkSpec::Cancun,
                seal_engine: SealEngine::NoProof,
            },
        )]));
        let input = serde_json::to_string(&suite).unwrap();
        let mut hook = NoopHook;
        execute_str(
            Path::new("simple-storage.json"),
            &input,
            ExecuteConfig { validate_post_state: true, mode, ..ExecuteConfig::default() },
            &EntryPoint::default(),
            &mut hook,
        )
        .unwrap()
    }

    #[cfg(feature = "jit")]
    #[test]
    fn jit_and_aot_modes_match_interpreter_for_simple_block() {
        let interpreter = execute_simple_storage_block(ExecutionMode::Interpreter);
        let jit = execute_simple_storage_block(ExecutionMode::Jit);
        let aot = execute_simple_storage_block(ExecutionMode::Aot);

        assert_eq!(interpreter.executed, 1);
        assert_eq!(jit.executed, interpreter.executed);
        assert_eq!(jit.skipped, interpreter.skipped);
        assert_eq!(aot.executed, interpreter.executed);
        assert_eq!(aot.skipped, interpreter.skipped);
    }
}
