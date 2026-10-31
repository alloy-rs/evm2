use super::{
    error::{TestError, TestErrorKind},
    hook::{
        BlockFailed, BlockFinished, BlockStarted, CaseStarted, Hook, NoopHook, TransactionFailed,
        TransactionFinished, TransactionStarted,
    },
    types::{
        Account, BlobFork, BlobScheduleEntry, Block, BlockHeader, BlockchainTest,
        BlockchainTestCase, ForkSpec, Transaction, Withdrawal,
    },
};
#[cfg(feature = "jit")]
use crate::compiled::{self, FileSummary};
use crate::{
    dump,
    execute::state_root_from_database,
    execution::ExecutionResources,
    filter::NameFilter,
    fixture_io,
    forks::is_fork_skipped,
    state::{insert_account_with_storage, parse_bytecode},
    trace::{self, Eip3155Tracer},
    tx::{
        TxFields, TxSigner, build_recovered_tx, rpc_access_list, signature_from_vrs,
        signed_authorizations,
    },
};
use alloy_consensus::{Eip658Value, Receipt, ReceiptEnvelope, Transaction as _, TxType};
use alloy_eip7928::{BlockAccessList, validate_block_access_list};
use alloy_eips::{
    eip2718::Typed2718,
    eip4844::DATA_GAS_PER_BLOB,
    eip6110::{DEPOSIT_REQUEST_TYPE, MAINNET_DEPOSIT_CONTRACT_ADDRESS},
    eip7002::WITHDRAWAL_REQUEST_TYPE,
    eip7251::CONSOLIDATION_REQUEST_TYPE,
    eip7685::Requests,
    eip7840::{BLOB_BASE_COST, BlobParams},
};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, Log, U256, aliases::U512, b256};
use alloy_rpc_types_eth::AccessList as RpcAccessList;
use anstyle::{AnsiColor, Color, Style};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResult,
    env::{BlockEnv, BlockEnvExt},
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{
        AccountChangeRef, AccountInfo as EvmAccountInfo, BEACON_ROOTS_ADDRESS, Bal,
        BlockStateAccumulator, DbStats, DbStatsCounts, HISTORY_STORAGE_ADDRESS, InMemoryDB,
        StateChangeSink, StateChangeSource, SystemTx, Tee, WITHDRAWAL_REQUEST_ADDRESS,
    },
    registry::HandlerError,
};
#[cfg(feature = "jit")]
use std::path::PathBuf;
use std::{collections::BTreeMap, mem, path::Path};

pub use crate::execution::ExecutionMode;

const ONE_GWEI: u64 = 1_000_000_000;
const ONE_ETHER: u128 = 1_000_000_000_000_000_000;

/// EIP-7934 maximum RLP-encoded block size: `MAX_BLOCK_SIZE` (10 MiB) minus `SAFETY_MARGIN`
/// (2 MiB).
const MAX_RLP_BLOCK_SIZE: usize = 10 * 1024 * 1024 - 2 * 1024 * 1024;

/// First topic of the deposit contract's `DepositEvent`.
const DEPOSIT_EVENT_SIGNATURE_HASH: B256 =
    b256!("0x649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5");

/// ABI layout of a `DepositEvent` payload: the offset of each dynamic field's length word and
/// the field's size, in the order the consensus layer consumes them (public key, withdrawal
/// credentials, amount, signature, index).
const DEPOSIT_EVENT_FIELDS: [(usize, usize); 5] =
    [(160, 48), (256, 32), (320, 8), (384, 96), (512, 8)];

/// Length of an ABI-encoded `DepositEvent` payload.
const DEPOSIT_EVENT_LENGTH: usize = 576;

/// EIP-8282 builder deposit request type.
const BUILDER_DEPOSIT_REQUEST_TYPE: u8 = 0x03;

/// EIP-8282 builder exit request type.
const BUILDER_EXIT_REQUEST_TYPE: u8 = 0x04;

/// Execution options for a single suite.
#[derive(Clone, Copy, Debug)]
pub struct ExecuteConfig {
    /// Whether to run the harness checks that are not part of execution: comparing the final
    /// state with the fixture's `postState` (unexpected live accounts and non-zero storage are
    /// rejected) and recovering each sender from its signature. When false, the runner trusts
    /// the fixture's `sender`.
    pub validate: bool,
    /// Whether to recompute each committed block's state root and compare it to
    /// the block header's `stateRoot`.
    pub compare_state_root: bool,
    /// Whether to recompute each block's receipt trie root and compare it to
    /// the block header's `receiptTrie`.
    pub compare_receipt_root: bool,
    /// Whether to stream EIP-3155 struct logs to stdout during execution.
    ///
    /// Only observable under [`ExecutionMode::Interpreter`]; the JIT/AOT runners
    /// bypass instruction-level dispatch.
    pub trace: bool,
    /// Whether to print a JSON outcome record per executed case (pass/fail,
    /// fork, and final state root).
    pub print_json_outcome: bool,
    /// Whether to dump the final post-execution state (accounts and storage) to
    /// stdout after the last block.
    pub dump_state: bool,
    /// Execution backend.
    pub mode: ExecutionMode,
    /// Whether to print database method call counts.
    pub db_stats: bool,
}

impl Default for ExecuteConfig {
    fn default() -> Self {
        Self {
            validate: true,
            compare_state_root: false,
            compare_receipt_root: false,
            trace: false,
            print_json_outcome: false,
            dump_state: false,
            mode: ExecutionMode::Interpreter,
            db_stats: false,
        }
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
    let test_filter = NameFilter::default();
    let mut hook = NoopHook;
    execute_suite(path, &suite, config, &test_filter, &mut hook)
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
        let test_filter = NameFilter::default();
        let mut hook = NoopHook;
        let file_summary = execute_suite_with_resources(
            &path,
            &suite,
            config,
            &test_filter,
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
    test_filter: &NameFilter,
    hook: &mut dyn Hook,
) -> Result<ExecuteSummary, TestError> {
    let suite: BlockchainTest =
        serde_json::from_str(input).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_suite(path, &suite, config, test_filter, hook)
}

/// Executes a parsed blockchain test suite.
pub fn execute_suite(
    path: &Path,
    suite: &BlockchainTest,
    config: ExecuteConfig,
    test_filter: &NameFilter,
    hook: &mut dyn Hook,
) -> Result<ExecuteSummary, TestError> {
    let resources =
        ExecutionResources::new(config.mode).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_suite_with_resources(path, suite, config, test_filter, hook, &resources)
}

/// Builds the pre-state database of a blockchain test case, including its seeded block hashes.
pub fn pre_state(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
) -> Result<InMemoryDB, TestError> {
    let mut database =
        parse_state(&test_case.pre.0).map_err(|err| TestError::case(path, name, err))?;
    seed_block_hashes(&mut database, test_case);
    Ok(database)
}

/// Executes a blockchain test case on `database`, which must hold the case's [`pre_state`].
///
/// Returns the final state root when `config` needs it.
pub fn execute_case_on(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
    database: &mut InMemoryDB,
    config: ExecuteConfig,
    hook: &mut dyn Hook,
) -> Result<Option<B256>, TestError> {
    let resources =
        ExecutionResources::new(config.mode).map_err(|err| TestError::unknown(path, err.into()))?;
    execute_case(path, name, test_case, database, config, hook, &resources)
}

fn execute_suite_with_resources(
    path: &Path,
    suite: &BlockchainTest,
    config: ExecuteConfig,
    test_filter: &NameFilter,
    hook: &mut dyn Hook,
    resources: &ExecutionResources,
) -> Result<ExecuteSummary, TestError> {
    let mut summary = ExecuteSummary::default();
    for (name, test_case) in &suite.0 {
        if !test_filter.matches(name)
            || fork_schedule(test_case.network).is_some_and(|schedule| schedule.is_skipped())
        {
            summary.skipped += 1;
            continue;
        }
        let outcome = pre_state(path, name, test_case).and_then(|mut database| {
            execute_case(path, name, test_case, &mut database, config, hook, resources)
        });
        if config.print_json_outcome {
            print_json_outcome(name, test_case.network, &outcome);
        }
        outcome?;
        summary.executed += 1;
    }
    Ok(summary)
}

/// Emits one JSON outcome record for a finished case (pass/fail, fork, root).
fn print_json_outcome(name: &str, network: ForkSpec, outcome: &Result<Option<B256>, TestError>) {
    println!("{}", json_outcome_value(name, network, outcome));
}

/// Builds the per-case JSON outcome record.
fn json_outcome_value(
    name: &str,
    network: ForkSpec,
    outcome: &Result<Option<B256>, TestError>,
) -> serde_json::Value {
    let (pass, state_root, error) = match outcome {
        Ok(state_root) => (true, state_root.unwrap_or(B256::ZERO), String::new()),
        Err(err) => (false, B256::ZERO, err.to_string()),
    };
    serde_json::json!({
        "test": name,
        "fork": network,
        "pass": pass,
        "stateRoot": state_root,
        "errorMsg": error,
    })
}

fn execute_case(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
    database: &mut InMemoryDB,
    config: ExecuteConfig,
    hook: &mut dyn Hook,
    resources: &ExecutionResources,
) -> Result<Option<B256>, TestError> {
    let schedule = case_schedule(path, name, test_case)?;
    let spec = schedule.initial.spec;
    let mut parent_block_hash = Some(test_case.genesis_block_header.hash);
    let mut parent_header = &test_case.genesis_block_header;
    let mut block_env = block_env_from_header(parent_header, &schedule)
        .map_err(|err| TestError::case(path, name, err))?;
    let total_blocks = test_case.blocks.len();
    let mut db_stats_counts = DbStatsCounts::default();
    // `evm2 capture` keeps only the accounts the blocks touch and zeroes the genesis state
    // root, so its blocks' state roots can't be reproduced. Receipt roots still check them.
    let captured = test_case.genesis_block_header.state_root.is_zero();
    let compare_state_root = config.compare_state_root && !captured;

    hook.case_started(CaseStarted { name, total_blocks, network: test_case.network });

    for (block_index, block) in test_case.blocks.iter().enumerate() {
        let block_number = block_number(block);
        let block_gas_used = block_gas_used(block);
        let total_transactions = block_transactions(block).len();
        let spec = block_header(block)
            .map_or(spec, |header| schedule.spec_at(header.number, header.timestamp));
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
            &schedule,
            spec,
            database,
            &mut block_env,
            &mut parent_block_hash,
            &mut parent_header,
            captured,
            hook,
            resources,
            config.trace,
            config.validate,
            compare_state_root,
            config.compare_receipt_root,
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

    resources.check_compilations().map_err(|err| TestError::case(path, name, err.into()))?;

    let final_block_hash = parent_block_hash.expect("genesis block hash should be available");
    if final_block_hash != test_case.lastblockhash {
        return Err(TestError::case(
            path,
            name,
            TestErrorKind::LastBlockHashMismatch {
                got: final_block_hash,
                expected: test_case.lastblockhash,
            },
        ));
    }

    if config.validate
        && let Some(expected) = &test_case.post_state
    {
        validate_post_state(database, expected).map_err(|err| TestError::case(path, name, err))?;
    }
    // Recompute the final state root only when a consumer needs it.
    let state_root = (config.print_json_outcome || config.dump_state)
        .then(|| state_root_from_database(database));
    if config.dump_state {
        dump::dump_state(&mut std::io::stdout(), database, state_root.unwrap_or_default());
    }
    if config.db_stats {
        print_db_stats(db_stats_counts);
    }
    Ok(state_root)
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
fn execute_block<'a>(
    path: &Path,
    name: &str,
    block_index: usize,
    total_blocks: usize,
    block_number: Option<U256>,
    total_transactions: usize,
    block: &'a Block,
    schedule: &ForkSchedule,
    spec: SpecId,
    database: &mut InMemoryDB,
    block_env: &mut BlockEnv,
    parent_block_hash: &mut Option<B256>,
    parent_header: &mut &'a BlockHeader,
    captured: bool,
    hook: &mut dyn Hook,
    resources: &ExecutionResources,
    trace: bool,
    validate: bool,
    compare_state_root: bool,
    compare_receipt_root: bool,
    db_stats_counts: Option<&mut DbStatsCounts>,
) -> Result<(), TestError> {
    let db_stats = db_stats_counts.is_some();
    let should_fail = block.expect_exception.is_some();

    if let Some(expected_bal) = block_access_list(block)
        && let Err(err) = validate_block_access_list(expected_bal, total_transactions)
    {
        if should_fail {
            return Ok(());
        }
        return Err(TestError::case(
            path,
            name,
            TestErrorKind::UnexpectedFailure(format!("invalid expected block access list: {err}")),
        ));
    }

    let mut block_hash = None;
    let mut beacon_root = None;
    let mut expected_state_root = None;
    let mut blob_params = None;

    let mut next_block_env = *block_env;
    if let Some(header) = block_header(block) {
        block_hash = Some(header.hash);
        beacon_root = header.parent_beacon_block_root;
        next_block_env = block_env_from_header(header, schedule)
            .map_err(|err| TestError::case(path, name, err))?;
        blob_params = schedule
            .blob_params(header.number, header.timestamp)
            .map_err(|err| TestError::case(path, name, err))?
            // The reserve price of EIP-7918 comes from the spec, not from the fixture's schedule.
            .map(|params| {
                if spec.enables(SpecId::OSAKA) {
                    params.with_blob_base_cost(BLOB_BASE_COST)
                } else {
                    params
                }
            });
        expected_state_root = Some(header.state_root);
    }
    // The blob checks multiply the blob counts by the blob gas, which must not overflow.
    if let Some(params) = &blob_params
        && (params.max_blob_count == 0
            || params.max_blob_count < params.target_blob_count
            || params.max_blob_count.checked_mul(DATA_GAS_PER_BLOB).is_none())
    {
        return Err(TestError::case(
            path,
            name,
            TestErrorKind::UnexpectedFailure(format!(
                "invalid blob schedule: max {} and target {} must satisfy target <= max, max > 0, and \
                 fit in blob gas",
                params.max_blob_count, params.target_blob_count
            )),
        ));
    }

    let initial_database = mem::take(database);
    let receipt_base = (compare_receipt_root && !spec.enables(SpecId::BYZANTIUM))
        .then(|| initial_database.clone());
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
    if trace {
        evm.set_inspector(Eip3155Tracer::to_stdout());
    }
    let mut block_state = BlockStateAccumulator::new();

    // When the fixture declares an EIP-7928 block access list, build one alongside execution and
    // validate it against the fixture. The block access index follows the EIP-7928 layout: index 0
    // for pre-block system calls, `i + 1` for transaction `i`, and the final index for post-block
    // rewards/withdrawals. Only Amsterdam+ fixtures carry a block access list, so pre-Amsterdam
    // blocks pay nothing.
    let build_bal = block_access_list(block).is_some();
    if build_bal {
        evm.state_mut().enable_bal_builder();
        evm.state_mut().reset_bal_index();
    }

    let result = (|| -> Result<BlockResolution, TestError> {
        if let Some(header) = block_header(block)
            && let Some(expected) = *parent_block_hash
            && header.parent_hash != expected
        {
            if should_fail {
                return Ok(BlockResolution::Discard);
            }
            return Err(TestError::case(
                path,
                name,
                TestErrorKind::ParentBlockHashMismatch { got: header.parent_hash, expected },
            ));
        }

        // A captured case's genesis header is the first block's own header, not its parent's.
        let parent = (!(captured && block_index == 0)).then_some(*parent_header);
        if let Some(header) = block_header(block)
            && let Err(reason) =
                validate_block_header(block, header, parent, blob_params.as_ref(), spec)
        {
            return reject_block(should_fail, path, name, reason);
        }

        if let Err(err) = pre_block_system_calls(
            &mut evm,
            &mut block_state,
            spec,
            next_block_env,
            *parent_block_hash,
            beacon_root,
        ) {
            // A failed pre-block system call invalidates the block only when the fixture expects
            // an exception; otherwise it is a test failure.
            if should_fail {
                return Ok(BlockResolution::Discard);
            }
            return Err(TestError::case(path, name, err));
        }

        let transactions = block_transactions(block);
        // EIP-8037: track execution and state gas separately for the block-header gas check.
        let mut cumulative_tx_gas_used = 0u64;
        let mut block_execution_gas_used = 0u64;
        let mut block_state_gas_used = 0u64;
        let mut block_blob_gas_used = 0u64;
        let mut receipts = Vec::with_capacity(transactions.len());
        let mut requests = Requests::default();
        let mut deposit_requests = Vec::new();
        for (transaction_index, raw_tx) in transactions.iter().enumerate() {
            hook.transaction_started(TransactionStarted {
                block_index,
                total_blocks,
                block_number,
                transaction_index,
                total_transactions,
            });
            let tx = match build_tx(raw_tx, spec, validate) {
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

            // Block-level inclusion check (execution-specs `check_transaction`): the transaction's
            // gas limit must fit the block gas remaining, and its blobs the block blob gas
            // remaining. Under EIP-8037 (Amsterdam+) the gas limit is checked per dimension:
            // capped by the EIP-7825 limit on the execution dimension, uncapped on the state
            // dimension.
            if let Some(header) = block_header(block) {
                let block_gas_limit = header.gas_limit.saturating_to::<u64>();
                let tx_gas_limit = tx.gas_limit();
                let (execution_gas_used, state_gas_used, execution_gas_limit) =
                    if spec.enables(SpecId::AMSTERDAM) {
                        (
                            block_execution_gas_used,
                            block_state_gas_used,
                            tx_gas_limit.min(evm.version().tx_gas_limit_cap),
                        )
                    } else {
                        (cumulative_tx_gas_used, cumulative_tx_gas_used, tx_gas_limit)
                    };
                let execution_gas_available = block_gas_limit.saturating_sub(execution_gas_used);
                let state_gas_available = block_gas_limit.saturating_sub(state_gas_used);
                if execution_gas_limit > execution_gas_available
                    || tx_gas_limit > state_gas_available
                {
                    return reject_block(
                        should_fail,
                        path,
                        name,
                        format!(
                            "transaction gas limit {tx_gas_limit} exceeds available block gas \
                             (execution {execution_gas_available}, state {state_gas_available})"
                        ),
                    );
                }

                if let Some(blob_params) = &blob_params {
                    let blob_gas = blob_gas_used(raw_tx);
                    let blob_gas_available =
                        blob_params.max_blob_gas_per_block().saturating_sub(block_blob_gas_used);
                    if blob_gas > blob_gas_available {
                        return reject_block(
                            should_fail,
                            path,
                            name,
                            format!(
                                "transaction blob gas {blob_gas} exceeds available block blob \
                                 gas {blob_gas_available}"
                            ),
                        );
                    }
                    block_blob_gas_used += blob_gas;
                }
            }

            // Transaction `i` is recorded at block access index `i + 1`.
            if build_bal {
                evm.state_mut().bump_bal_index();
            }
            match execute_tx(&mut evm, &mut block_state, &tx) {
                Ok(result) => {
                    cumulative_tx_gas_used =
                        cumulative_tx_gas_used.saturating_add(result.tx_gas_used());
                    block_execution_gas_used =
                        block_execution_gas_used.saturating_add(result.execution_gas_spent());
                    block_state_gas_used =
                        block_state_gas_used.saturating_add(result.state_gas_spent());
                    hook.transaction_finished(TransactionFinished {
                        block_index,
                        total_blocks,
                        block_number,
                        transaction_index,
                        total_transactions,
                        gas_used: result.tx_gas_used(),
                        execution_gas_used: result.execution_gas_spent(),
                        state_gas_used: result.state_gas_spent(),
                        success: result.status,
                        logs: &result.logs,
                    });
                    if spec.enables(SpecId::PRAGUE)
                        && let Err(reason) =
                            collect_deposit_requests(&result.logs, &mut deposit_requests)
                    {
                        return reject_block(should_fail, path, name, reason);
                    }
                    if compare_receipt_root {
                        let status = if spec.enables(SpecId::BYZANTIUM) {
                            Eip658Value::Eip658(result.status)
                        } else {
                            let mut post_state = receipt_base
                                .as_ref()
                                .expect("pre-Byzantium receipt base should be available")
                                .clone();
                            post_state.commit_source(&block_state);
                            Eip658Value::PostState(state_root_from_database(&post_state))
                        };
                        let receipt = Receipt {
                            status,
                            cumulative_gas_used: cumulative_tx_gas_used,
                            logs: result.logs,
                        }
                        .with_bloom();
                        let tx_type = TxType::try_from(tx.inner().ty())
                            .expect("executor only builds supported Ethereum transaction types");
                        receipts.push(ReceiptEnvelope::from_typed(tx_type, receipt));
                    }
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

        // Validate the block header's gas used against the executed transactions. Under EIP-8037
        // (Amsterdam+) execution and state gas are tracked separately and the header records their
        // max; earlier forks record the cumulative per-transaction gas used (refunds included).
        // Block-validation failures on a block that expects an exception resolve to a discard,
        // mirroring execution-specs raising `InvalidBlock`.
        if let Some(expected) = block_gas_used(block) {
            let expected = expected.saturating_to::<u64>();
            let actual = if spec.enables(SpecId::AMSTERDAM) {
                block_execution_gas_used.max(block_state_gas_used)
            } else {
                cumulative_tx_gas_used
            };
            if actual != expected {
                if should_fail {
                    return Ok(BlockResolution::Discard);
                }
                return Err(TestError::case(
                    path,
                    name,
                    TestErrorKind::BlockGasUsedMismatch { expected, actual },
                ));
            }
        }

        if spec.enables(SpecId::CANCUN)
            && let Some(header) = block_header(block)
            && header.blob_gas_used != Some(U256::from(block_blob_gas_used))
        {
            return reject_block(
                should_fail,
                path,
                name,
                format!(
                    "block blob gas used mismatch: header {:?}, computed {block_blob_gas_used}",
                    header.blob_gas_used
                ),
            );
        }

        if compare_receipt_root && let Some(header) = block_header(block) {
            let got = alloy_consensus::proofs::calculate_receipt_root(&receipts);
            if got != header.receipt_trie {
                if should_fail {
                    return Ok(BlockResolution::Discard);
                }
                return Err(TestError::case(
                    path,
                    name,
                    TestErrorKind::ReceiptRootMismatch { got, expected: header.receipt_trie },
                ));
            }
        }

        // Post-block rewards and withdrawals are recorded at the final (post-execution) index.
        if build_bal {
            evm.state_mut().bump_bal_index();
        }

        requests.push_request_with_type(DEPOSIT_REQUEST_TYPE, deposit_requests);
        if let Err(err) = post_block_transition(
            &mut evm,
            &mut block_state,
            spec,
            next_block_env,
            block_ommers(block),
            block_withdrawals(block),
            captured,
            &mut requests,
        ) {
            // A failed post-block system call (e.g. SYSTEM_CONTRACT_CALL_FAILED) invalidates the
            // block, mirroring execution-specs raising `InvalidBlock`.
            if should_fail {
                return Ok(BlockResolution::Discard);
            }
            return Err(TestError::case(path, name, err));
        }

        // A captured case has no system contracts, so its requests omit the system call outputs.
        if !captured
            && spec.enables(SpecId::PRAGUE)
            && let Some(header) = block_header(block)
        {
            let computed = requests.requests_hash();
            if header.requests_hash != Some(computed) {
                return reject_block(
                    should_fail,
                    path,
                    name,
                    format!(
                        "requests hash mismatch: header {:?}, computed {computed}",
                        header.requests_hash
                    ),
                );
            }
        }

        if let Some(expected_bal) = block_access_list(block) {
            let built = evm.state_mut().take_bal_builder().unwrap_or_default();
            if let Err(kind) =
                check_block_access_list(block_index, built, expected_bal, block_header(block))
            {
                if should_fail {
                    return Ok(BlockResolution::Discard);
                }
                return Err(TestError::case(path, name, kind));
            }
        }

        if should_fail {
            let expected = block.expect_exception.clone().unwrap_or_default();
            return Err(TestError::case(path, name, TestErrorKind::UnexpectedSuccess(expected)));
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
            if let Some(header) = block_header(block) {
                *parent_header = header;
            }
            if trace || compare_state_root {
                let got = state_root_from_database(database);
                if trace {
                    let gas_used =
                        block_gas_used(block).map(|gas| gas.saturating_to::<u64>()).unwrap_or(0);
                    trace::write_summary(&mut std::io::stdout(), &Bytes::new(), gas_used, got);
                }
                if compare_state_root
                    && let Some(expected) = expected_state_root
                    && got != expected
                {
                    return Err(TestError::case(
                        path,
                        name,
                        TestErrorKind::StateRootMismatch { got, expected },
                    ));
                }
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

/// Returns the block's EIP-7928 access list, falling back to the decoded RLP payload: invalid-BAL
/// fixtures carry the corrupted list only under `rlp_decoded`.
fn block_access_list(block: &Block) -> Option<&BlockAccessList> {
    block.block_access_list.as_ref().or_else(|| {
        block.rlp_decoded.as_ref().and_then(|decoded| decoded.block_access_list.as_ref())
    })
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

fn block_ommers(block: &Block) -> &[BlockHeader] {
    if let Some(ommers) = &block.uncle_headers
        && !ommers.is_empty()
    {
        return ommers;
    }
    block.rlp_decoded.as_ref().map(|decoded| decoded.uncle_headers.as_slice()).unwrap_or_default()
}

/// Block-level validity checks that execution-specs performs before executing the transactions:
/// the EIP-7934 block RLP size limit and the EIP-4844 header blob fields.
fn validate_block_header(
    block: &Block,
    header: &BlockHeader,
    parent: Option<&BlockHeader>,
    blob_params: Option<&BlobParams>,
    spec: SpecId,
) -> Result<(), String> {
    if spec.enables(SpecId::OSAKA) && block.rlp.len() > MAX_RLP_BLOCK_SIZE {
        return Err(format!(
            "block RLP size {} exceeds the limit {MAX_RLP_BLOCK_SIZE}",
            block.rlp.len()
        ));
    }

    // The blob parameters are `None` before Cancun.
    let Some(blob_params) = blob_params else {
        if header.excess_blob_gas.is_some() || header.blob_gas_used.is_some() {
            return Err("pre-Cancun block header carries blob gas fields".to_string());
        }
        return Ok(());
    };

    if header.blob_gas_used.is_none() {
        return Err("block header is missing blobGasUsed".to_string());
    }
    let Some(parent) = parent else { return Ok(()) };
    let parent_excess = parent.excess_blob_gas.unwrap_or_default();
    let parent_used = parent.blob_gas_used.unwrap_or_default();
    // Reject parent values whose sum the excess blob gas formula cannot add without overflow.
    if parent_excess.saturating_add(parent_used) > U256::from(u64::MAX) {
        return Err(format!(
            "parent blob gas fields out of range: excess {parent_excess}, used {parent_used}"
        ));
    }
    let parent_base_fee = parent.base_fee_per_gas.unwrap_or_default();
    let expected_excess_blob_gas =
        next_excess_blob_gas(blob_params, parent_excess.to(), parent_used.to(), parent_base_fee);
    if header.excess_blob_gas != Some(U256::from(expected_excess_blob_gas)) {
        return Err(format!(
            "excess blob gas mismatch: header {:?}, expected {expected_excess_blob_gas}",
            header.excess_blob_gas
        ));
    }
    Ok(())
}

/// Returns the excess blob gas of the block after a parent with these values (execution-specs
/// `calculate_excess_blob_gas`, with the EIP-7918 reserve price from Osaka). Unlike
/// [`BlobParams::next_block_excess_blob_gas_osaka`], the reserve price comparison takes a base
/// fee of any width.
fn next_excess_blob_gas(
    params: &BlobParams,
    parent_excess: u64,
    parent_used: u64,
    parent_base_fee: U256,
) -> u64 {
    let excess = parent_excess + parent_used;
    let target = params.target_blob_gas_per_block();
    if excess < target {
        return 0;
    }
    if reserve_price_exceeds_blob_price(params, parent_excess, parent_base_fee) {
        // The scaled blob gas is at most `parent_used`, so the sum stays below `u64::MAX`.
        let scaled_used = u128::from(parent_used)
            * u128::from(params.max_blob_count - params.target_blob_count)
            / u128::from(params.max_blob_count);
        parent_excess + scaled_used as u64
    } else {
        excess - target
    }
}

/// Returns whether `blob_base_cost * base_fee > DATA_GAS_PER_BLOB * blob_price` for the blob price
/// at this excess blob gas, in unbounded integer arithmetic like execution-specs. The blob price
/// is the EIP-4844 `fake_exponential` sum, which this stops accumulating once it reaches the
/// reserve price, so every product stays far below the 512-bit limit.
fn reserve_price_exceeds_blob_price(
    params: &BlobParams,
    excess_blob_gas: u64,
    base_fee: U256,
) -> bool {
    let reserve_price = U512::from(params.blob_base_cost) * U512::from(base_fee);
    let denominator = U512::from(params.update_fraction);
    let numerator = U512::from(excess_blob_gas);
    let gas_per_blob = U512::from(DATA_GAS_PER_BLOB);
    let mut term = U512::from(params.min_blob_fee) * denominator;
    let mut sum = U512::ZERO;
    let mut index = U512::ONE;
    while !term.is_zero() {
        sum += term;
        if gas_per_blob * (sum / denominator) >= reserve_price {
            return false;
        }
        term = term * numerator / (denominator * index);
        index += U512::ONE;
    }
    reserve_price > gas_per_blob * (sum / denominator)
}

/// Resolves an invalid block: it is discarded when the fixture expects an exception, and a test
/// failure otherwise.
fn reject_block(
    should_fail: bool,
    path: &Path,
    name: &str,
    reason: String,
) -> Result<BlockResolution, TestError> {
    if should_fail {
        Ok(BlockResolution::Discard)
    } else {
        Err(TestError::case(path, name, TestErrorKind::UnexpectedFailure(reason)))
    }
}

/// Returns the blob gas a transaction consumes (EIP-4844).
const fn blob_gas_used(tx: &Transaction) -> u64 {
    tx.blob_versioned_hashes.len() as u64 * DATA_GAS_PER_BLOB
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
    if spec.enables(SpecId::CANCUN)
        && let Some(root) = parent_beacon_block_root
    {
        run_system_call(
            evm,
            block_state,
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(root.as_slice()),
            "eip4788",
            SystemCallCheck::None,
        )?;
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
            SystemCallCheck::None,
        )?;
    }
    Ok(())
}

#[expect(clippy::too_many_arguments)]
fn post_block_transition(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    spec: SpecId,
    block: BlockEnv,
    ommers: &[BlockHeader],
    withdrawals: &[Withdrawal],
    captured: bool,
    requests: &mut Requests,
) -> Result<(), TestErrorKind> {
    // A captured case keeps only the accounts its blocks touch, so the system contracts are absent.
    let check = if captured { SystemCallCheck::Outcome } else { SystemCallCheck::OutcomeAndCode };
    let reward = block_reward(spec, ommers.len());
    if reward != 0 {
        increment_balance(evm, block_state, block.beneficiary, U256::from(reward))?;

        let base_reward = U256::from(base_block_reward(spec));
        for ommer in ommers {
            let Some(age) = block.number.checked_sub(ommer.number) else {
                return Err(TestErrorKind::InvalidOmmerAge {
                    block_number: block.number,
                    ommer_number: ommer.number,
                });
            };
            if age.is_zero() || age > U256::from(6) {
                return Err(TestErrorKind::InvalidOmmerAge {
                    block_number: block.number,
                    ommer_number: ommer.number,
                });
            }
            let reward = (U256::from(8) - age) * base_reward / U256::from(8);
            increment_balance(evm, block_state, ommer.coinbase, reward)?;
        }
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
        let output = run_system_call(
            evm,
            block_state,
            WITHDRAWAL_REQUEST_ADDRESS,
            Bytes::new(),
            "eip7002",
            check,
        )?;
        requests.push_request_with_type(WITHDRAWAL_REQUEST_TYPE, output);
        let output = run_system_call(
            evm,
            block_state,
            evm2::evm::CONSOLIDATION_REQUEST_ADDRESS,
            Bytes::new(),
            "eip7251",
            check,
        )?;
        requests.push_request_with_type(CONSOLIDATION_REQUEST_TYPE, output);
    }

    // EIP-8282: builder deposit (request type 0x03) and builder exit (0x04) requests, Amsterdam+.
    if spec.enables(SpecId::AMSTERDAM) {
        let output = run_system_call(
            evm,
            block_state,
            evm2::evm::BUILDER_DEPOSIT_REQUEST_ADDRESS,
            Bytes::new(),
            "eip8282_deposit",
            check,
        )?;
        requests.push_request_with_type(BUILDER_DEPOSIT_REQUEST_TYPE, output);
        let output = run_system_call(
            evm,
            block_state,
            evm2::evm::BUILDER_EXIT_REQUEST_ADDRESS,
            Bytes::new(),
            "eip8282_exit",
            check,
        )?;
        requests.push_request_with_type(BUILDER_EXIT_REQUEST_TYPE, output);
    }
    Ok(())
}

/// What a system call must satisfy for the block to be valid.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SystemCallCheck {
    /// The call may fail (EIP-4788, EIP-2935).
    None,
    /// The call must succeed.
    Outcome,
    /// The target must have code, and the call must succeed (execution-specs
    /// `process_checked_system_transaction`).
    OutcomeAndCode,
}

fn run_system_call(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    address: Address,
    data: Bytes,
    label: &'static str,
    check: SystemCallCheck,
) -> Result<Bytes, TestErrorKind> {
    // A checked call to a target without code is an invalid block (execution-specs
    // `SYSTEM_CONTRACT_EMPTY`); the EVM itself treats it as a successful no-op.
    if check == SystemCallCheck::OutcomeAndCode {
        let info = evm
            .read_account_info(&address)
            .map_err(|code| TestErrorKind::UnexpectedFailure(code.to_string()))?;
        if info.is_none_or(|info| info.is_empty_code_hash()) {
            return Err(TestErrorKind::UnexpectedFailure(format!(
                "{label} system contract has no code"
            )));
        }
    }
    let executed = evm.system_call(SystemTx::new(address, data))?;
    if !executed.result().status {
        // A failed unchecked call keeps the state accesses it recorded, which a BAL lists as reads.
        if check == SystemCallCheck::None {
            let _ = executed.commit_to(block_state);
            return Ok(Bytes::new());
        }
        let _ = executed.discard();
        return Err(TestErrorKind::SystemCall(label));
    }
    Ok(executed.commit_to(block_state).output)
}

/// Appends the unframed fields of every deposit event in `logs` to `deposit_requests`
/// (execution-specs `parse_deposit_requests`).
fn collect_deposit_requests(logs: &[Log], deposit_requests: &mut Vec<u8>) -> Result<(), String> {
    for log in logs {
        if log.address == MAINNET_DEPOSIT_CONTRACT_ADDRESS
            && log.topics().first() == Some(&DEPOSIT_EVENT_SIGNATURE_HASH)
        {
            extract_deposit_data(&log.data.data, deposit_requests)?;
        }
    }
    Ok(())
}

/// Strips the ABI framing from a `DepositEvent` payload, appending the fields to `out`
/// (execution-specs `extract_deposit_data`).
fn extract_deposit_data(data: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    if data.len() != DEPOSIT_EVENT_LENGTH {
        return Err("invalid deposit event data length".to_string());
    }
    let word = |offset: usize| U256::from_be_slice(&data[offset..offset + 32]);
    for (index, (offset, _)) in DEPOSIT_EVENT_FIELDS.into_iter().enumerate() {
        if word(index * 32) != U256::from(offset) {
            return Err(format!("invalid offset for deposit field {index}"));
        }
    }
    for (index, (offset, size)) in DEPOSIT_EVENT_FIELDS.into_iter().enumerate() {
        if word(offset) != U256::from(size) {
            return Err(format!("invalid size for deposit field {index}"));
        }
        out.extend_from_slice(&data[offset + 32..offset + 32 + size]);
    }
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
            original: self.original.as_ref(),
            current: self.current.as_ref(),
            created: false,
            selfdestructed: false,
        })
    }
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

fn block_env_from_header(
    header: &BlockHeader,
    schedule: &ForkSchedule,
) -> Result<BlockEnv, TestErrorKind> {
    let excess_blob_gas = header.excess_blob_gas.unwrap_or_default().saturating_to::<u64>();
    let blob_basefee = schedule
        .blob_params(header.number, header.timestamp)?
        .map_or(U256::ONE, |params| U256::from(params.calc_blob_fee(excess_blob_gas)));
    Ok(BlockEnvExt {
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
        blob_basefee,
        slot_num: header.slot_number.unwrap_or_default(),
        ext: (),
        _non_exhaustive: (),
    })
}

fn build_tx(
    raw: &Transaction,
    spec: SpecId,
    validate: bool,
) -> Result<RecoveredTxEnvelope, TestErrorKind> {
    let gas_limit = raw.gas_limit.try_into().map_err(|_| TestErrorKind::Overflow("gasLimit"))?;
    let nonce = raw.nonce.try_into().map_err(|_| TestErrorKind::Overflow("nonce"))?;
    let tx_type = raw.tx_type();
    let (signer, chain_id) = if validate {
        let (signature, legacy_chain_id) =
            signature_from_vrs(raw.r, raw.s, raw.v, tx_type == 0, spec)?;
        // A legacy transaction signs its chain ID through `v`, so `v` decides it.
        let chain_id = if tx_type == 0 { legacy_chain_id.map(U256::from) } else { raw.chain_id };
        (TxSigner::Signature { signature, spec }, chain_id)
    } else {
        (TxSigner::Fixture(raw.sender.ok_or(TestErrorKind::MissingSender)?), raw.chain_id)
    };

    let tx = build_recovered_tx(
        TxFields {
            tx_type: Some(tx_type),
            kind: raw.kind(),
            data: raw.data.clone(),
            gas_limit,
            nonce,
            value: raw.value,
            chain_id,
            gas_price: raw.gas_price,
            max_fee_per_gas: raw.max_fee_per_gas,
            max_priority_fee_per_gas: raw.max_priority_fee_per_gas,
            access_list: access_list(raw)?,
            authorization_list: signed_authorizations(raw.authorization_list.as_deref())?,
            blob_versioned_hashes: raw.blob_versioned_hashes.clone(),
            max_fee_per_blob_gas: raw.max_fee_per_blob_gas,
        },
        signer,
    )?;
    if validate
        && let Some(expected) = raw.sender
        && expected != tx.signer()
    {
        return Err(TestErrorKind::SenderMismatch { recovered: tx.signer(), expected });
    }
    Ok(tx)
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
    let reward = base_block_reward(spec);
    reward + (reward >> 5) * ommers as u128
}

const fn base_block_reward(spec: SpecId) -> u128 {
    if spec.enables(SpecId::MERGE) {
        0
    } else if spec.enables(SpecId::PETERSBURG) {
        ONE_ETHER * 2
    } else if spec.enables(SpecId::BYZANTIUM) {
        ONE_ETHER * 3
    } else {
        ONE_ETHER * 5
    }
}

fn increment_balance(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    address: Address,
    amount: U256,
) -> Result<(), TestErrorKind> {
    let original = match evm.read_account_info(&address) {
        Ok(info) => info,
        Err(code) => return Err(TestErrorKind::UnexpectedFailure(code.to_string())),
    };
    let mut current = original.clone().unwrap_or_default();
    current.balance = current.balance.saturating_add(amount);
    if current.code_hash.is_zero() {
        current.code_hash = KECCAK256_EMPTY;
    }
    // EELS `modify_state` destroys an account left empty by the modification, so a zero-amount
    // withdrawal neither creates its recipient nor keeps a pre-existing empty one alive. The
    // BAL commit below still runs to record the touched address.
    let current = (!current.is_empty()).then_some(current);

    let change = AccountStateChange { address, original, current };
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
    // `Some` denotes a live account, including an empty account retained before state clearing.
    // Cached absent/deleted accounts (`None`) are not part of the final allocation.
    for (address, info) in &database.cache.accounts {
        if info.is_some() && !expected.contains_key(address) {
            return Err(TestErrorKind::UnexpectedFailure(format!(
                "unexpected account in post-state: {address}"
            )));
        }
    }
    // Storage is cached independently of account info, so also catch storage-only entries.
    for (address, storage) in &database.cache.storage {
        if !expected.contains_key(address) {
            for (key, value) in &storage.slots {
                if !value.is_zero() {
                    return Err(TestErrorKind::UnexpectedFailure(format!(
                        "unexpected storage for {address}[{key}]: got {value}, expected 0"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Validates the block access list built during execution: the EIP-7928 item-count bound, the
/// header's block access list hash, and a comparison against the fixture's expected list.
///
/// The built list is canonicalized into EIP-7928 order (`From<Bal> for BlockAccessList`) before
/// comparison. The fixture-provided list must already be canonical.
fn check_block_access_list(
    block_index: usize,
    built: Bal,
    expected: &BlockAccessList,
    header: Option<&BlockHeader>,
) -> Result<(), TestErrorKind> {
    let built = BlockAccessList::from(built);

    // EIP-7928: the total number of items (addresses + unique storage keys) is bounded by the
    // block gas limit (execution-specs `validate_block_access_list_gas_limit`).
    if let Some(header) = header {
        let items: u64 = built
            .iter()
            .map(|account| {
                1 + account.storage_changes.len() as u64 + account.storage_reads.len() as u64
            })
            .sum();
        let max_items = header.gas_limit.saturating_to::<u64>() / alloy_eip7928::ITEM_COST as u64;
        if items > max_items {
            return Err(TestErrorKind::UnexpectedFailure(format!(
                "block access list exceeds gas limit: {items} items exceeds limit of {max_items}"
            )));
        }
    }

    // The header commits to the computed list by hash (execution-specs compares
    // `hash_block_access_list` against `header.block_access_list_hash`).
    if let Some(expected_hash) = header.and_then(|header| header.block_access_list_hash) {
        let computed_hash = alloy_eip7928::compute_block_access_list_hash(&built);
        if computed_hash != expected_hash {
            return Err(TestErrorKind::UnexpectedFailure(format!(
                "block access list hash mismatch: computed {computed_hash}, header {expected_hash}"
            )));
        }
    }

    if built.as_slice() == expected.as_slice() {
        return Ok(());
    }
    Err(TestErrorKind::BlockAccessListMismatch {
        block_index,
        details: format_bal_diff(&built, expected),
    })
}

/// Renders a compact per-account summary of how the built and expected block access lists differ.
fn format_bal_diff(
    built: &alloy_eip7928::BlockAccessList,
    expected: &alloy_eip7928::BlockAccessList,
) -> String {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fmt::Write,
    };

    let built: BTreeMap<_, _> = built.iter().map(|account| (account.address, account)).collect();
    let expected: BTreeMap<_, _> =
        expected.iter().map(|account| (account.address, account)).collect();
    let addresses: BTreeSet<_> = built.keys().chain(expected.keys()).copied().collect();

    let mut out = String::new();
    for address in addresses {
        match (built.get(&address), expected.get(&address)) {
            (Some(_), None) => {
                let _ = write!(out, "\n  {address}: only in built");
            }
            (None, Some(_)) => {
                let _ = write!(out, "\n  {address}: only in expected");
            }
            (Some(built), Some(expected)) if built != expected => {
                let _ = write!(out, "\n  {address}: differs");
                if built.nonce_changes != expected.nonce_changes {
                    let _ = write!(
                        out,
                        " nonce (built {:?}, expected {:?})",
                        built.nonce_changes, expected.nonce_changes
                    );
                }
                if built.balance_changes != expected.balance_changes {
                    let _ = write!(
                        out,
                        " balance (built {:?}, expected {:?})",
                        built.balance_changes, expected.balance_changes
                    );
                }
                if built.code_changes != expected.code_changes {
                    let _ = write!(out, " code");
                }
                if built.storage_changes != expected.storage_changes {
                    let _ = write!(
                        out,
                        " storage_writes (built {:?}, expected {:?})",
                        built.storage_changes, expected.storage_changes
                    );
                }
                if built.storage_reads != expected.storage_reads {
                    let _ = write!(
                        out,
                        " storage_reads (built {:?}, expected {:?})",
                        built.storage_reads, expected.storage_reads
                    );
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether a blockchain test targeting `spec` should be skipped.
///
/// Only the globally unsupported forks ([`is_fork_skipped`]) are skipped. Amsterdam blockchain
/// tests are executed: EIP-7928 block access lists are built and validated (see
/// [`check_block_access_list`]) and EIP-8037 block-level gas is checked in [`execute_block`].
fn is_blockchain_fork_skipped(spec: SpecId) -> bool {
    is_fork_skipped(spec)
}

/// The specs a fork runs, in order.
struct ForkSchedule {
    initial: ForkStage,
    /// The stage that a transition fork switches to, and when.
    transition: Option<(Activation, ForkStage)>,
    /// The test's blob parameters by fork.
    blob_schedule: BTreeMap<BlobFork, BlobScheduleEntry>,
}

/// What is active between two transitions: a spec, and the fork that sets the blob parameters.
#[derive(Clone, Copy)]
struct ForkStage {
    spec: SpecId,
    /// `None` before Cancun.
    blob_fork: Option<BlobFork>,
}

/// When a transition fork activates.
#[derive(Clone, Copy)]
enum Activation {
    Block(u64),
    Timestamp(u64),
}

impl ForkStage {
    /// A stage whose blob fork is the latest one the spec enables.
    const fn new(spec: SpecId) -> Self {
        let blob_fork = if spec.enables(SpecId::AMSTERDAM) {
            Some(BlobFork::Amsterdam)
        } else if spec.enables(SpecId::OSAKA) {
            Some(BlobFork::Osaka)
        } else if spec.enables(SpecId::PRAGUE) {
            Some(BlobFork::Prague)
        } else if spec.enables(SpecId::CANCUN) {
            Some(BlobFork::Cancun)
        } else {
            None
        };
        Self { spec, blob_fork }
    }

    /// A stage of a blob parameters only fork, which keeps the Osaka spec.
    const fn bpo(blob_fork: BlobFork) -> Self {
        Self { spec: SpecId::OSAKA, blob_fork: Some(blob_fork) }
    }
}

impl ForkSchedule {
    const fn fixed(spec: SpecId) -> Self {
        Self::new(ForkStage::new(spec), None)
    }

    const fn transition(initial: SpecId, activation: Activation, spec: SpecId) -> Self {
        Self::new(ForkStage::new(initial), Some((activation, ForkStage::new(spec))))
    }

    const fn new(initial: ForkStage, transition: Option<(Activation, ForkStage)>) -> Self {
        Self { initial, transition, blob_schedule: BTreeMap::new() }
    }

    /// Returns the stage of the block with this number and timestamp.
    fn stage_at(&self, number: U256, timestamp: U256) -> ForkStage {
        match self.transition {
            Some((Activation::Block(at), stage)) if number >= U256::from(at) => stage,
            Some((Activation::Timestamp(at), stage)) if timestamp >= U256::from(at) => stage,
            _ => self.initial,
        }
    }

    /// Returns the spec of the block with this number and timestamp.
    fn spec_at(&self, number: U256, timestamp: U256) -> SpecId {
        self.stage_at(number, timestamp).spec
    }

    /// Returns the blob parameters of the block with this number and timestamp, or `None` before
    /// Cancun.
    fn blob_params(
        &self,
        number: U256,
        timestamp: U256,
    ) -> Result<Option<BlobParams>, TestErrorKind> {
        let Some(blob_fork) = self.stage_at(number, timestamp).blob_fork else { return Ok(None) };
        let entry = self
            .blob_schedule
            .get(&blob_fork)
            .ok_or(TestErrorKind::MissingBlobSchedule(blob_fork))?;
        if entry.base_fee_update_fraction.is_zero() {
            return Err(TestErrorKind::ZeroBlobUpdateFraction(blob_fork));
        }
        Ok(Some(entry.params()))
    }

    /// Returns whether any spec of the fork is skipped.
    fn is_skipped(&self) -> bool {
        is_blockchain_fork_skipped(self.initial.spec)
            || self.transition.is_some_and(|(_, stage)| is_blockchain_fork_skipped(stage.spec))
    }
}

/// Returns the fork schedule of a test case, with the blob parameters from its `config`.
fn case_schedule(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
) -> Result<ForkSchedule, TestError> {
    let mut schedule = fork_schedule(test_case.network).ok_or_else(|| {
        TestError::case(path, name, TestErrorKind::UnsupportedFork(test_case.network))
    })?;
    if let Some(config) = &test_case.config {
        schedule.blob_schedule.clone_from(&config.blob_schedule);
    }
    Ok(schedule)
}

/// Returns the blob parameters of the block with this header in a test case, or `None` before
/// Cancun.
///
/// The parameters come from the case's `config.blobSchedule`, and the fork that is active at the
/// header's number and timestamp.
pub fn blob_params(
    path: &Path,
    name: &str,
    test_case: &BlockchainTestCase,
    header: &BlockHeader,
) -> Result<Option<BlobParams>, TestError> {
    case_schedule(path, name, test_case)?
        .blob_params(header.number, header.timestamp)
        .map_err(|err| TestError::case(path, name, err))
}

/// Returns the specs of a fork, or `None` for the DAO fork, which the runner does not support.
const fn fork_schedule(fork: ForkSpec) -> Option<ForkSchedule> {
    use Activation::{Block, Timestamp};
    const AT_5: Activation = Block(5);
    const AT_15K: Activation = Timestamp(15_000);

    Some(match fork {
        ForkSpec::Frontier => ForkSchedule::fixed(SpecId::FRONTIER),
        ForkSpec::Homestead => ForkSchedule::fixed(SpecId::HOMESTEAD),
        ForkSpec::EIP150 => ForkSchedule::fixed(SpecId::TANGERINE),
        ForkSpec::EIP158 => ForkSchedule::fixed(SpecId::SPURIOUS_DRAGON),
        ForkSpec::Byzantium => ForkSchedule::fixed(SpecId::BYZANTIUM),
        ForkSpec::Constantinople | ForkSpec::ConstantinopleFix => {
            ForkSchedule::fixed(SpecId::PETERSBURG)
        }
        ForkSpec::Istanbul => ForkSchedule::fixed(SpecId::ISTANBUL),
        ForkSpec::Berlin => ForkSchedule::fixed(SpecId::BERLIN),
        ForkSpec::London => ForkSchedule::fixed(SpecId::LONDON),
        ForkSpec::Paris
        | ForkSpec::MergeEOF
        | ForkSpec::MergeMeterInitCode
        | ForkSpec::MergePush0 => ForkSchedule::fixed(SpecId::MERGE),
        ForkSpec::Shanghai => ForkSchedule::fixed(SpecId::SHANGHAI),
        ForkSpec::Cancun => ForkSchedule::fixed(SpecId::CANCUN),
        ForkSpec::Prague => ForkSchedule::fixed(SpecId::PRAGUE),
        ForkSpec::Osaka => ForkSchedule::fixed(SpecId::OSAKA),
        ForkSpec::BPO1 => ForkSchedule::new(ForkStage::bpo(BlobFork::Bpo1), None),
        ForkSpec::BPO2 => ForkSchedule::new(ForkStage::bpo(BlobFork::Bpo2), None),
        ForkSpec::Amsterdam => ForkSchedule::fixed(SpecId::AMSTERDAM),
        ForkSpec::FrontierToHomesteadAt5 => {
            ForkSchedule::transition(SpecId::FRONTIER, AT_5, SpecId::HOMESTEAD)
        }
        ForkSpec::HomesteadToEIP150At5 => {
            ForkSchedule::transition(SpecId::HOMESTEAD, AT_5, SpecId::TANGERINE)
        }
        ForkSpec::EIP158ToByzantiumAt5 => {
            ForkSchedule::transition(SpecId::SPURIOUS_DRAGON, AT_5, SpecId::BYZANTIUM)
        }
        ForkSpec::ByzantiumToConstantinopleAt5 | ForkSpec::ByzantiumToConstantinopleFixAt5 => {
            ForkSchedule::transition(SpecId::BYZANTIUM, AT_5, SpecId::PETERSBURG)
        }
        ForkSpec::BerlinToLondonAt5 => {
            ForkSchedule::transition(SpecId::BERLIN, AT_5, SpecId::LONDON)
        }
        ForkSpec::ParisToShanghaiAtTime15k => {
            ForkSchedule::transition(SpecId::MERGE, AT_15K, SpecId::SHANGHAI)
        }
        ForkSpec::ShanghaiToCancunAtTime15k => {
            ForkSchedule::transition(SpecId::SHANGHAI, AT_15K, SpecId::CANCUN)
        }
        ForkSpec::CancunToPragueAtTime15k => {
            ForkSchedule::transition(SpecId::CANCUN, AT_15K, SpecId::PRAGUE)
        }
        ForkSpec::PragueToOsakaAtTime15k => {
            ForkSchedule::transition(SpecId::PRAGUE, AT_15K, SpecId::OSAKA)
        }
        ForkSpec::OsakaToBPO1AtTime15k => ForkSchedule::new(
            ForkStage::new(SpecId::OSAKA),
            Some((AT_15K, ForkStage::bpo(BlobFork::Bpo1))),
        ),
        ForkSpec::BPO1ToBPO2AtTime15k => ForkSchedule::new(
            ForkStage::bpo(BlobFork::Bpo1),
            Some((AT_15K, ForkStage::bpo(BlobFork::Bpo2))),
        ),
        ForkSpec::BPO2ToBPO3AtTime15k => ForkSchedule::new(
            ForkStage::bpo(BlobFork::Bpo2),
            Some((AT_15K, ForkStage::bpo(BlobFork::Bpo3))),
        ),
        ForkSpec::BPO3ToBPO4AtTime15k => ForkSchedule::new(
            ForkStage::bpo(BlobFork::Bpo3),
            Some((AT_15K, ForkStage::bpo(BlobFork::Bpo4))),
        ),
        ForkSpec::BPO2ToAmsterdamAtTime15k => ForkSchedule::new(
            ForkStage::bpo(BlobFork::Bpo2),
            Some((AT_15K, ForkStage::new(SpecId::AMSTERDAM))),
        ),
        ForkSpec::HomesteadToDaoAt5 => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        super::types::{ChainConfig, SealEngine, State},
        *,
    };
    #[cfg(feature = "jit")]
    use crate::tx::recover_address;
    #[cfg(feature = "jit")]
    use alloy_consensus::{SignableTransaction, TxLegacy, crypto::secp256k1};
    #[cfg(feature = "jit")]
    use alloy_primitives::{TxKind, to_eip155_v};
    #[cfg(feature = "jit")]
    use evm2::interpreter::op;
    use std::collections::BTreeMap;

    #[cfg(feature = "jit")]
    const BYTECODE_STORE42: &[u8] = &[op::PUSH1, 0x42, op::PUSH0, op::SSTORE, op::STOP];

    fn cancun_config() -> ChainConfig {
        ChainConfig {
            chainid: U256::ONE,
            blob_schedule: BTreeMap::from([(
                BlobFork::Cancun,
                BlobScheduleEntry::from(BlobParams::cancun()),
            )]),
        }
    }

    fn test_case(network: ForkSpec, config: Option<ChainConfig>) -> BlockchainTestCase {
        BlockchainTestCase {
            genesis_block_header: BlockHeader::default(),
            genesis_rlp: None,
            blocks: Vec::new(),
            post_state: None,
            pre: State(BTreeMap::new()),
            block_hashes: Vec::new(),
            lastblockhash: B256::ZERO,
            network,
            seal_engine: SealEngine::NoProof,
            config,
        }
    }

    #[test]
    fn blob_params_switch_at_the_transition_fork() {
        let config = ChainConfig {
            chainid: U256::ONE,
            blob_schedule: BTreeMap::from([
                (BlobFork::Bpo1, BlobScheduleEntry::from(BlobParams::bpo1())),
                (BlobFork::Bpo2, BlobScheduleEntry::from(BlobParams::bpo2())),
            ]),
        };
        let case = test_case(ForkSpec::BPO1ToBPO2AtTime15k, Some(config));
        let blob_params = |timestamp| {
            let header = BlockHeader { timestamp: U256::from(timestamp), ..Default::default() };
            blob_params(Path::new("test.json"), "test", &case, &header).unwrap().unwrap()
        };
        assert_eq!(blob_params(14_999).update_fraction, BlobParams::bpo1().update_fraction);
        assert_eq!(blob_params(15_000).update_fraction, BlobParams::bpo2().update_fraction);
    }

    #[test]
    fn blob_params_require_a_schedule_entry_from_cancun() {
        let case = test_case(ForkSpec::Cancun, None);
        let err = blob_params(Path::new("test.json"), "test", &case, &BlockHeader::default())
            .unwrap_err();
        assert!(matches!(err.kind, TestErrorKind::MissingBlobSchedule(BlobFork::Cancun)));
    }

    #[test]
    fn blob_params_reject_a_zero_update_fraction() {
        let config = ChainConfig {
            chainid: U256::ONE,
            blob_schedule: BTreeMap::from([(
                BlobFork::Cancun,
                BlobScheduleEntry {
                    target: U256::from(3),
                    max: U256::from(6),
                    base_fee_update_fraction: U256::ZERO,
                },
            )]),
        };
        let case = test_case(ForkSpec::Cancun, Some(config));
        let err = blob_params(Path::new("test.json"), "test", &case, &BlockHeader::default())
            .unwrap_err();
        assert!(matches!(err.kind, TestErrorKind::ZeroBlobUpdateFraction(BlobFork::Cancun)));
    }

    #[test]
    fn unsupported_fork_fails_instead_of_skipping() {
        let suite = BlockchainTest(BTreeMap::from([(
            "dao".to_string(),
            test_case(ForkSpec::HomesteadToDaoAt5, None),
        )]));
        let err = execute_suite(
            Path::new("dao.json"),
            &suite,
            ExecuteConfig::default(),
            &NameFilter::default(),
            &mut NoopHook,
        )
        .unwrap_err();
        assert!(matches!(err.kind, TestErrorKind::UnsupportedFork(ForkSpec::HomesteadToDaoAt5)));
    }

    #[test]
    fn post_state_rejects_omitted_live_accounts() {
        let address = Address::with_last_byte(1);
        for account in [
            Account { balance: U256::ONE, ..Default::default() },
            Account { nonce: U256::ONE, ..Default::default() },
            Account { code: Bytes::from_static(&[0x00]), ..Default::default() },
            // Empty but live accounts also contribute to the state trie.
            Account::default(),
        ] {
            let database = parse_state(&BTreeMap::from([(address, account)])).unwrap();
            let err = validate_post_state(&database, &BTreeMap::new()).unwrap_err();
            assert!(matches!(err, TestErrorKind::UnexpectedFailure(message)
                if message == format!("unexpected account in post-state: {address}")));
        }
    }

    #[test]
    fn post_state_rejects_omitted_storage_without_live_account() {
        let address = Address::with_last_byte(1);
        for cached_absent in [false, true] {
            let mut database = InMemoryDB::default();
            if cached_absent {
                database.cache.accounts.insert(address, None);
            }
            database.insert_account_storage(&address, &U256::ZERO, &U256::ONE);
            let err = validate_post_state(&database, &BTreeMap::new()).unwrap_err();
            assert!(matches!(err, TestErrorKind::UnexpectedFailure(message)
                if message == format!("unexpected storage for {address}[0]: got 1, expected 0")));
        }
    }

    #[test]
    fn post_state_accepts_complete_allocation_with_absent_and_zero_storage_cache_entries() {
        let address = Address::with_last_byte(1);
        let absent = Address::with_last_byte(2);
        let storage_only = Address::with_last_byte(3);
        let expected = BTreeMap::from([(
            address,
            Account {
                balance: U256::ONE,
                nonce: U256::ONE,
                code: Bytes::from_static(&[0x00]),
                storage: BTreeMap::from([(U256::ZERO, U256::ONE)]),
            },
        )]);
        let mut database = parse_state(&expected).unwrap();
        database.cache.accounts.insert(absent, None);
        for address in [address, absent, storage_only] {
            database.insert_account_storage(&address, &U256::ONE, &U256::ZERO);
        }
        validate_post_state(&database, &expected).unwrap();
    }

    #[test]
    fn post_state_checks_all_expected_account_fields() {
        let address = Address::with_last_byte(1);
        let expected = BTreeMap::from([(address, Account::default())]);
        for (account, mismatch) in [
            (Account { balance: U256::ONE, ..Default::default() }, "balance mismatch"),
            (Account { nonce: U256::ONE, ..Default::default() }, "nonce mismatch"),
            (Account { code: Bytes::from_static(&[0x00]), ..Default::default() }, "code mismatch"),
            (
                Account {
                    storage: BTreeMap::from([(U256::ZERO, U256::ONE)]),
                    ..Default::default()
                },
                "unexpected storage",
            ),
        ] {
            let database = parse_state(&BTreeMap::from([(address, account)])).unwrap();
            let err = validate_post_state(&database, &expected).unwrap_err();
            assert!(
                matches!(err, TestErrorKind::UnexpectedFailure(message)
                if message.starts_with(mismatch)),
                "expected {mismatch}"
            );
        }
    }

    #[test]
    fn blockchain_post_state_is_full_validation_by_default() {
        let address = Address::with_last_byte(1);
        let mut suite = BlockchainTest(BTreeMap::from([(
            "omitted-account".to_string(),
            BlockchainTestCase {
                genesis_block_header: BlockHeader::default(),
                genesis_rlp: None,
                blocks: Vec::new(),
                post_state: Some(BTreeMap::new()),
                pre: State(BTreeMap::from([(
                    address,
                    Account { balance: U256::ONE, ..Default::default() },
                )])),
                block_hashes: Vec::new(),
                lastblockhash: B256::ZERO,
                network: ForkSpec::Paris,
                seal_engine: SealEngine::NoProof,
                config: None,
            },
        )]));
        let run = |suite: &BlockchainTest, config| {
            execute_suite(
                Path::new("omitted-account.json"),
                suite,
                config,
                &NameFilter::default(),
                &mut NoopHook,
            )
        };
        let err = run(&suite, ExecuteConfig::default()).unwrap_err();
        assert!(matches!(err.kind, TestErrorKind::UnexpectedFailure(message)
            if message == format!("unexpected account in post-state: {address}")));

        let summary = run(&suite, ExecuteConfig { validate: false, ..Default::default() }).unwrap();
        assert_eq!(summary.executed, 1);

        let case = suite.0.get_mut("omitted-account").unwrap();
        case.post_state = Some(case.pre.0.clone());
        assert_eq!(run(&suite, ExecuteConfig::default()).unwrap().executed, 1);
    }

    #[test]
    fn blockchain_tests_apply_ommer_rewards_before_merge() {
        use super::{
            super::types::{
                Account, Block, BlockHeader, BlockchainTest, BlockchainTestCase, DecodedBlock,
                ForkSpec, SealEngine, State,
            },
            ExecuteConfig, NoopHook, ONE_ETHER, execute_suite,
        };
        use crate::filter::NameFilter;
        use alloy_primitives::{Address, B256, U256};
        use std::{collections::BTreeMap, path::Path};

        let miner = Address::with_last_byte(1);
        let ommer_miner = Address::with_last_byte(2);
        let genesis_hash = B256::with_last_byte(1);
        let block_hash = B256::with_last_byte(2);

        for (fork, base_reward) in [
            (ForkSpec::Homestead, 5 * ONE_ETHER),
            (ForkSpec::Byzantium, 3 * ONE_ETHER),
            (ForkSpec::ConstantinopleFix, 2 * ONE_ETHER),
            (ForkSpec::Paris, 0),
        ] {
            let mut post_state = BTreeMap::new();
            if base_reward != 0 {
                post_state.insert(
                    miner,
                    Account {
                        balance: U256::from(base_reward + base_reward / 32),
                        ..Default::default()
                    },
                );
                post_state.insert(
                    ommer_miner,
                    Account { balance: U256::from(base_reward * 7 / 8), ..Default::default() },
                );
            }

            let block_header = BlockHeader {
                parent_hash: genesis_hash,
                hash: block_hash,
                coinbase: miner,
                number: U256::ONE,
                gas_limit: U256::from(30_000_000),
                timestamp: U256::ONE,
                ..Default::default()
            };
            let ommer_header =
                BlockHeader { coinbase: ommer_miner, number: U256::ZERO, ..Default::default() };
            let block = if fork == ForkSpec::Byzantium {
                Block {
                    rlp_decoded: Some(DecodedBlock {
                        block_header: Some(block_header),
                        uncle_headers: vec![ommer_header],
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            } else {
                Block {
                    block_header: Some(block_header),
                    uncle_headers: Some(vec![ommer_header]),
                    ..Default::default()
                }
            };

            let suite = BlockchainTest(BTreeMap::from([(
                "ommer-rewards".to_string(),
                BlockchainTestCase {
                    genesis_block_header: BlockHeader {
                        hash: genesis_hash,
                        gas_limit: U256::from(30_000_000),
                        ..Default::default()
                    },
                    blocks: vec![block],
                    post_state: Some(post_state),
                    pre: State(BTreeMap::new()),
                    block_hashes: Vec::new(),
                    lastblockhash: block_hash,
                    network: fork,
                    seal_engine: SealEngine::NoProof,
                    config: None,
                    genesis_rlp: None,
                },
            )]));

            let summary = execute_suite(
                Path::new("ommer-rewards.json"),
                &suite,
                ExecuteConfig::default(),
                &NameFilter::default(),
                &mut NoopHook,
            )
            .unwrap();
            assert_eq!(summary.executed, 1, "fork {fork:?}");
        }
    }

    #[test]
    fn receipt_root_check_rejects_invalid_empty_block_root() {
        use super::{
            super::types::{
                Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, SealEngine, State,
            },
            ExecuteConfig, ExecutionMode, NoopHook, execute_str,
        };
        use crate::filter::NameFilter;
        use alloy_primitives::{B256, U256};
        use std::{collections::BTreeMap, path::Path};

        let genesis_hash = B256::with_last_byte(1);
        let suite = BlockchainTest(BTreeMap::from([(
            "invalid-receipt-root".to_string(),
            BlockchainTestCase {
                genesis_block_header: BlockHeader {
                    hash: genesis_hash,
                    gas_limit: U256::from(30_000_000),
                    ..Default::default()
                },
                blocks: vec![Block {
                    block_header: Some(BlockHeader {
                        parent_hash: genesis_hash,
                        hash: B256::with_last_byte(2),
                        number: U256::ONE,
                        gas_limit: U256::from(30_000_000),
                        receipt_trie: B256::ZERO,
                        excess_blob_gas: Some(U256::ZERO),
                        blob_gas_used: Some(U256::ZERO),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                post_state: None,
                pre: State(BTreeMap::new()),
                block_hashes: Vec::new(),
                lastblockhash: B256::with_last_byte(2),
                network: ForkSpec::Cancun,
                seal_engine: SealEngine::NoProof,
                config: Some(cancun_config()),
                genesis_rlp: None,
            },
        )]));
        let input = serde_json::to_string(&suite).unwrap();
        let error = execute_str(
            Path::new("invalid-receipt-root.json"),
            &input,
            ExecuteConfig {
                compare_receipt_root: true,
                mode: ExecutionMode::Interpreter,
                ..Default::default()
            },
            &NameFilter::default(),
            &mut NoopHook,
        )
        .unwrap_err();

        assert!(matches!(error.kind, super::TestErrorKind::ReceiptRootMismatch { .. }));
    }

    #[test]
    fn next_excess_blob_gas_matches_blob_params_and_keeps_full_width_base_fees() {
        let params = BlobParams::osaka();
        for excess in [0, 786_432, 5_000_000, 249_036_800, 400_000_000] {
            for used in [0, 131_072, 786_432] {
                for base_fee in [0, 1, 1_000_000_000, u64::MAX] {
                    assert_eq!(
                        next_excess_blob_gas(&params, excess, used, U256::from(base_fee)),
                        params.next_block_excess_blob_gas_osaka(excess, used, base_fee),
                    );
                }
            }
        }

        // The reserve price, 2^256, exceeds the blob price, so the reserve branch applies.
        assert_eq!(next_excess_blob_gas(&params, 786_432, 0, U256::ONE << 243), 786_432);
        // The blob price exceeds `u128`, and 2^145 times it still exceeds this reserve price, so
        // the target subtraction applies.
        assert_eq!(
            next_excess_blob_gas(&params, 1_000_000_000, 786_432, U256::ONE << 140),
            1_000_000_000,
        );
    }

    #[test]
    fn rejects_block_with_wrong_parent_hash() {
        use super::{
            super::types::{
                Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, SealEngine, State,
            },
            ExecuteConfig, NoopHook, execute_str,
        };
        use crate::filter::NameFilter;
        use alloy_primitives::{B256, U256};
        use std::{collections::BTreeMap, path::Path};

        let genesis_hash = B256::with_last_byte(1);
        let block_hash = B256::with_last_byte(2);
        let suite = BlockchainTest(BTreeMap::from([(
            "wrong-parent-hash".to_string(),
            BlockchainTestCase {
                genesis_block_header: BlockHeader {
                    hash: genesis_hash,
                    gas_limit: U256::from(30_000_000),
                    ..Default::default()
                },
                blocks: vec![Block {
                    block_header: Some(BlockHeader {
                        parent_hash: B256::with_last_byte(3),
                        hash: block_hash,
                        number: U256::ONE,
                        gas_limit: U256::from(30_000_000),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                post_state: None,
                pre: State(BTreeMap::new()),
                block_hashes: Vec::new(),
                lastblockhash: block_hash,
                network: ForkSpec::Cancun,
                seal_engine: SealEngine::NoProof,
                config: Some(cancun_config()),
                genesis_rlp: None,
            },
        )]));
        let input = serde_json::to_string(&suite).unwrap();
        let error = execute_str(
            Path::new("wrong-parent-hash.json"),
            &input,
            ExecuteConfig::default(),
            &NameFilter::default(),
            &mut NoopHook,
        )
        .unwrap_err();

        assert!(matches!(
            error.kind,
            super::TestErrorKind::ParentBlockHashMismatch { got, expected }
                if got == B256::with_last_byte(3) && expected == genesis_hash
        ));
    }

    #[test]
    fn discards_expected_block_with_wrong_parent_hash() {
        use super::{
            super::types::{
                Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, SealEngine, State,
            },
            ExecuteConfig, NoopHook, execute_str,
        };
        use crate::filter::NameFilter;
        use alloy_primitives::{B256, U256};
        use std::{collections::BTreeMap, path::Path};

        let genesis_hash = B256::with_last_byte(1);
        let suite = BlockchainTest(BTreeMap::from([(
            "expected-wrong-parent-hash".to_string(),
            BlockchainTestCase {
                genesis_block_header: BlockHeader {
                    hash: genesis_hash,
                    gas_limit: U256::from(30_000_000),
                    ..Default::default()
                },
                blocks: vec![Block {
                    block_header: Some(BlockHeader {
                        parent_hash: B256::with_last_byte(3),
                        hash: B256::with_last_byte(2),
                        number: U256::ONE,
                        gas_limit: U256::from(30_000_000),
                        ..Default::default()
                    }),
                    expect_exception: Some("invalid parent hash".to_string()),
                    ..Default::default()
                }],
                post_state: None,
                pre: State(BTreeMap::new()),
                block_hashes: Vec::new(),
                lastblockhash: genesis_hash,
                network: ForkSpec::Cancun,
                seal_engine: SealEngine::NoProof,
                config: Some(cancun_config()),
                genesis_rlp: None,
            },
        )]));
        let input = serde_json::to_string(&suite).unwrap();

        execute_str(
            Path::new("expected-wrong-parent-hash.json"),
            &input,
            ExecuteConfig::default(),
            &NameFilter::default(),
            &mut NoopHook,
        )
        .unwrap();
    }

    #[test]
    fn rejects_case_with_wrong_last_block_hash() {
        use super::{
            super::types::{
                Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, SealEngine, State,
            },
            ExecuteConfig, NoopHook, execute_str,
        };
        use crate::filter::NameFilter;
        use alloy_primitives::{B256, U256};
        use std::{collections::BTreeMap, path::Path};

        let genesis_hash = B256::with_last_byte(1);
        let block_hash = B256::with_last_byte(2);
        let suite = BlockchainTest(BTreeMap::from([(
            "wrong-last-block-hash".to_string(),
            BlockchainTestCase {
                genesis_block_header: BlockHeader {
                    hash: genesis_hash,
                    gas_limit: U256::from(30_000_000),
                    ..Default::default()
                },
                blocks: vec![Block {
                    block_header: Some(BlockHeader {
                        parent_hash: genesis_hash,
                        hash: block_hash,
                        number: U256::ONE,
                        gas_limit: U256::from(30_000_000),
                        excess_blob_gas: Some(U256::ZERO),
                        blob_gas_used: Some(U256::ZERO),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                post_state: None,
                pre: State(BTreeMap::new()),
                block_hashes: Vec::new(),
                lastblockhash: genesis_hash,
                network: ForkSpec::Cancun,
                seal_engine: SealEngine::NoProof,
                config: Some(cancun_config()),
                genesis_rlp: None,
            },
        )]));
        let input = serde_json::to_string(&suite).unwrap();
        let error = execute_str(
            Path::new("wrong-last-block-hash.json"),
            &input,
            ExecuteConfig::default(),
            &NameFilter::default(),
            &mut NoopHook,
        )
        .unwrap_err();

        assert!(matches!(
            error.kind,
            super::TestErrorKind::LastBlockHashMismatch { got, expected }
                if got == block_hash && expected == genesis_hash
        ));
    }

    #[cfg(feature = "jit")]
    fn execute_simple_storage_block(mode: ExecutionMode) -> ExecuteSummary {
        let secret = B256::with_last_byte(1);
        let caller = recover_address(secret.as_slice()).unwrap();
        let target = Address::from([0x22; 20]);
        let unsigned = TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: 100_000,
            to: TxKind::Call(target),
            ..Default::default()
        };
        let signature = secp256k1::sign_message(secret, unsigned.signature_hash()).unwrap();
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
                        excess_blob_gas: Some(U256::ZERO),
                        blob_gas_used: Some(U256::ZERO),
                        ..BlockHeader::default()
                    }),
                    transactions: Some(vec![Transaction {
                        transaction_type: None,
                        sender: Some(caller),
                        data: Bytes::new(),
                        gas_limit: U256::from(100_000),
                        gas_price: Some(U256::ZERO),
                        nonce: U256::ZERO,
                        r: signature.r(),
                        s: signature.s(),
                        v: U256::from(to_eip155_v(signature.v() as u8, 1)),
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
                config: Some(cancun_config()),
            },
        )]));
        let input = serde_json::to_string(&suite).unwrap();
        let mut hook = NoopHook;
        execute_str(
            Path::new("simple-storage.json"),
            &input,
            ExecuteConfig { validate: true, mode, ..ExecuteConfig::default() },
            &NameFilter::default(),
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

    #[test]
    fn json_outcome_reports_pass_and_failure() {
        use super::{
            super::{
                error::{TestError, TestErrorKind},
                types::ForkSpec,
            },
            json_outcome_value,
        };
        use alloy_primitives::B256;

        let root = B256::with_last_byte(7);
        let pass = json_outcome_value("case-a", ForkSpec::Byzantium, &Ok(Some(root)));
        assert_eq!(pass["pass"], true);
        assert_eq!(pass["fork"], "Byzantium");
        assert_eq!(pass["stateRoot"], serde_json::to_value(root).unwrap());
        assert_eq!(pass["errorMsg"], "");

        let failure: Result<Option<B256>, TestError> = Err(TestError::case(
            std::path::Path::new("f.json"),
            "case-b",
            TestErrorKind::UnexpectedFailure("boom".to_string()),
        ));
        let fail = json_outcome_value("case-b", ForkSpec::Byzantium, &failure);
        assert_eq!(fail["pass"], false);
        assert_eq!(fail["stateRoot"], serde_json::to_value(B256::ZERO).unwrap());
        assert!(fail["errorMsg"].as_str().unwrap().contains("boom"));
    }
}
