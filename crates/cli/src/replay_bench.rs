//! Mainnet replay comparison using the EEST executor and revm.
//!
//! Both engines report results through the EEST hook. Parity checks collect
//! transaction outcomes; timed runs use a no-op hook and include database setup,
//! transaction decoding, execution and block commits.

use alloy_eips::{eip7702::SignedAuthorization, eip7840::BlobParams};
use alloy_primitives::{Address, B256, Bytes, Log, TxKind, U256, keccak256};
use alloy_rpc_types_eth::{AccessList as RpcAccessList, AccessListItem as RpcAccessListItem};
use evm2::{
    SpecId,
    evm::{
        BEACON_ROOTS_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS,
        WITHDRAWAL_REQUEST_ADDRESS,
    },
};
use evm2_eest::{
    BlockchainTestExecuteConfig, NameFilter,
    blockchaintest::{
        Block, BlockFinished, BlockHeader, BlockStarted, BlockchainTest, BlockchainTestCase,
        CaseStarted, ForkSpec, Hook, Transaction, TransactionFinished, TransactionStarted,
        Withdrawal,
    },
    execute_blockchain_tests_suite,
};
use revm::{
    Context, DatabaseCommit, ExecuteEvm, MainBuilder, MainContext, SystemCallEvm,
    context::{BlockEnv as RevmBlockEnv, CfgEnv, ContextTr, JournalTr, TxEnv},
    context_interface::{Cfg, block::BlobExcessGasAndPrice, either::Either},
    database::{CacheDB, EmptyDB, InMemoryDB as RevmInMemoryDB},
    handler::EvmTr,
    primitives::hardfork::SpecId as RevmSpecId,
    state::{AccountInfo as RevmAccountInfo, Bytecode as RevmBytecode},
};
use std::path::Path;

const ONE_GWEI: u64 = 1_000_000_000;

/// Per-transaction outcome recorded identically by both engines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxOutcome {
    /// Gas charged to the transaction (refunds applied).
    pub gas_used: u64,
    /// Whether execution finished successfully.
    pub success: bool,
    /// Number of logs emitted by the transaction.
    pub logs: usize,
    /// Digest of every emitted log's address, topics and data, in emission order.
    pub logs_digest: B256,
}

/// Per-block outcome recorded identically by both engines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockOutcome {
    /// Block number taken from the fixture header.
    pub number: u64,
    /// `gasUsed` declared by the fixture header.
    pub header_gas_used: u64,
    /// Cumulative receipt gas used by the block's transactions (refunds applied).
    pub gas_used: u64,
    /// Cumulative EIP-8037 execution gas (pre-refund, EIP-7623 floor applied).
    pub execution_gas_used: u64,
    /// Cumulative EIP-8037 state gas.
    pub state_gas_used: u64,
    /// Gas the header must record under the fixture's fork: [`Self::gas_used`] before
    /// Amsterdam, `max(execution_gas_used, state_gas_used)` from Amsterdam on, mirroring
    /// the EEST executor's block validation.
    pub block_gas_used: u64,
    /// Per-transaction outcomes, in block order.
    pub txs: Vec<TxOutcome>,
}

/// Outcome of replaying every block of a fixture case.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReplayOutcome {
    /// Per-block outcomes, in fixture order.
    pub blocks: Vec<BlockOutcome>,
}

impl ReplayOutcome {
    /// Returns the total number of executed transactions.
    pub fn transactions(&self) -> usize {
        self.blocks.iter().map(|block| block.txs.len()).sum()
    }

    /// Returns the total transaction gas used across every block.
    pub fn gas_used(&self) -> u128 {
        self.blocks.iter().map(|block| u128::from(block.gas_used)).sum()
    }

    /// Returns the blocks whose fork-rule block gas disagrees with the fixture header.
    pub fn header_gas_mismatches(&self) -> Vec<&BlockOutcome> {
        self.blocks.iter().filter(|block| block.block_gas_used != block.header_gas_used).collect()
    }
}

/// One disagreement between the two engines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mismatch {
    /// Block number the disagreement was found in.
    pub block: u64,
    /// Transaction index inside the block, or `None` for block-level fields.
    pub transaction: Option<usize>,
    /// Name of the compared field.
    pub field: &'static str,
    /// Value observed on the evm2 side.
    pub evm2: String,
    /// Value observed on the revm side.
    pub revm: String,
}

/// Compares two replay outcomes transaction by transaction.
pub fn diff(evm2: &ReplayOutcome, revm: &ReplayOutcome) -> Vec<Mismatch> {
    let mut mismatches = Vec::new();
    if evm2.blocks.len() != revm.blocks.len() {
        mismatches.push(Mismatch {
            block: 0,
            transaction: None,
            field: "block_count",
            evm2: evm2.blocks.len().to_string(),
            revm: revm.blocks.len().to_string(),
        });
        return mismatches;
    }
    for (left, right) in evm2.blocks.iter().zip(&revm.blocks) {
        let block = left.number;
        if left.number != right.number {
            mismatches.push(Mismatch {
                block,
                transaction: None,
                field: "block_number",
                evm2: left.number.to_string(),
                revm: right.number.to_string(),
            });
        }
        if left.txs.len() != right.txs.len() {
            mismatches.push(Mismatch {
                block,
                transaction: None,
                field: "transaction_count",
                evm2: left.txs.len().to_string(),
                revm: right.txs.len().to_string(),
            });
            continue;
        }
        for (field, lhs, rhs) in [
            ("block_gas_used", left.gas_used, right.gas_used),
            ("block_execution_gas_used", left.execution_gas_used, right.execution_gas_used),
            ("block_state_gas_used", left.state_gas_used, right.state_gas_used),
        ] {
            if lhs != rhs {
                mismatches.push(Mismatch {
                    block,
                    transaction: None,
                    field,
                    evm2: lhs.to_string(),
                    revm: rhs.to_string(),
                });
            }
        }
        for (index, (left, right)) in left.txs.iter().zip(&right.txs).enumerate() {
            if left.gas_used != right.gas_used {
                mismatches.push(Mismatch {
                    block,
                    transaction: Some(index),
                    field: "gas_used",
                    evm2: left.gas_used.to_string(),
                    revm: right.gas_used.to_string(),
                });
            }
            if left.success != right.success {
                mismatches.push(Mismatch {
                    block,
                    transaction: Some(index),
                    field: "success",
                    evm2: left.success.to_string(),
                    revm: right.success.to_string(),
                });
            }
            if left.logs != right.logs {
                mismatches.push(Mismatch {
                    block,
                    transaction: Some(index),
                    field: "logs",
                    evm2: left.logs.to_string(),
                    revm: right.logs.to_string(),
                });
            }
            if left.logs_digest != right.logs_digest {
                mismatches.push(Mismatch {
                    block,
                    transaction: Some(index),
                    field: "logs_digest",
                    evm2: left.logs_digest.to_string(),
                    revm: right.logs_digest.to_string(),
                });
            }
        }
    }
    mismatches
}

/// A decoded blockchain-replay fixture holding exactly one test case.
#[derive(Debug)]
pub struct ReplayFixture {
    name: String,
    spec: SpecId,
    suite: BlockchainTest,
}

impl ReplayFixture {
    /// Decodes the fixture at `path` and selects its single test case.
    ///
    /// # Panics
    ///
    /// Panics when the fixture cannot be read or does not contain exactly one case, or
    /// when the case fails the checks in [`Self::from_case`].
    pub fn load(path: &Path) -> Self {
        let suite: BlockchainTest = evm2_eest::read_blockchain_fixture(path)
            .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()));
        assert_eq!(
            suite.0.len(),
            1,
            "replay fixture {} must contain exactly one case",
            path.display()
        );
        let (name, case) = suite.0.into_iter().next().expect("fixture must contain a case");
        Self::from_case(name, case)
    }

    /// Wraps a decoded test case after checking that it is a canonical chain.
    ///
    /// The revm driver supports valid post-merge blocks without block access lists.
    ///
    /// # Panics
    ///
    /// Panics when the case has no blocks, when a block expects an exception, lacks a
    /// header or does not extend the previous block, or when `lastblockhash` is not the
    /// final block's hash. Pre-merge forks and block access lists are unsupported.
    pub fn from_case(name: String, case: BlockchainTestCase) -> Self {
        assert_canonical(&name, &case);
        let spec = fork_to_spec_id(case.network);
        assert!(spec.enables(SpecId::MERGE), "replay corpus must be post-merge");
        Self { name: name.clone(), spec, suite: BlockchainTest([(name, case)].into()) }
    }

    /// Returns the case name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the fork the case runs under.
    pub const fn spec(&self) -> SpecId {
        self.spec
    }

    /// Returns the number of blocks in the case.
    pub fn blocks(&self) -> usize {
        self.case().blocks.len()
    }

    /// Returns the total number of transactions across every block.
    pub fn transactions(&self) -> usize {
        self.case().blocks.iter().map(|block| block_transactions(block).len()).sum()
    }

    fn case(&self) -> &BlockchainTestCase {
        &self.suite.0[&self.name]
    }

    /// Replays through the benchmark's EEST executor and records transaction outcomes.
    pub fn replay_evm2(&self) -> ReplayOutcome {
        let mut recorder = ReplayRecorder { spec: self.spec, outcome: ReplayOutcome::default() };
        let summary = execute_blockchain_tests_suite(
            Path::new(&self.name),
            &self.suite,
            BlockchainTestExecuteConfig { validate_post_state: false, ..Default::default() },
            &NameFilter::default(),
            &mut recorder,
        )
        .expect("EEST replay must execute");
        assert_eq!(summary.executed, 1);
        assert_eq!(summary.skipped, 0);
        recorder.outcome
    }

    /// Replays through revm and records transaction outcomes for parity checks.
    pub fn replay_revm(&self) -> ReplayOutcome {
        let mut recorder = ReplayRecorder { spec: self.spec, outcome: ReplayOutcome::default() };
        self.execute_revm(&mut recorder);
        recorder.outcome
    }

    /// Replays every block through revm using the same block sequence and cadence.
    pub fn execute_revm(&self, hook: &mut dyn Hook) {
        let case = self.case();
        let spec = self.spec;
        let revm_spec = revm_spec_id(spec);
        let mut database = revm_pre_state(case);
        revm_seed_block_hashes(&mut database, case);

        let mut cfg = CfgEnv::new();
        cfg.set_spec_and_mainnet_gas_params(revm_spec);

        let mut parent_block_hash = Some(case.genesis_block_header.hash);
        let mut parent_excess_blob_gas =
            case.genesis_block_header.excess_blob_gas.unwrap_or_default().saturating_to::<u64>();
        let total_blocks = case.blocks.len();
        hook.case_started(CaseStarted { name: &self.name, total_blocks, network: case.network });

        for (block_index, block) in case.blocks.iter().enumerate() {
            let header = block_header(block).expect("replay fixture block must carry a header");
            let block_env = revm_block_env(header, parent_excess_blob_gas, spec);
            let block_number = header.number.saturating_to::<u64>();

            let transactions = block_transactions(block);
            let total_transactions = transactions.len();
            hook.block_started(BlockStarted {
                block_index,
                total_blocks,
                block_number: Some(header.number),
                block_gas_used: Some(header.gas_used),
                total_transactions,
            });
            let mut gas_used = 0u64;
            let mut execution_gas_used = 0u64;
            let mut state_gas_used = 0u64;
            {
                let mut evm = Context::mainnet()
                    .with_cfg(cfg.clone())
                    .with_block(block_env)
                    .with_db(&mut database)
                    .build_mainnet();

                revm_pre_block(&mut evm, spec, block_number, parent_block_hash, header);

                for (transaction_index, raw) in transactions.iter().enumerate() {
                    hook.transaction_started(TransactionStarted {
                        block_index,
                        total_blocks,
                        block_number: Some(header.number),
                        transaction_index,
                        total_transactions,
                    });
                    let tx = revm_tx(raw);
                    if spec.enables(SpecId::AMSTERDAM) {
                        let block_gas_limit = header.gas_limit.saturating_to::<u64>();
                        assert!(
                            tx.gas_limit.min(cfg.tx_gas_limit_cap())
                                <= block_gas_limit.saturating_sub(execution_gas_used)
                                && tx.gas_limit <= block_gas_limit.saturating_sub(state_gas_used),
                            "revm transaction gas limit exceeds available block gas",
                        );
                    }
                    let result = evm.transact_one(tx).unwrap_or_else(|err| {
                        panic!("revm replay transaction must execute: {err:?}")
                    });
                    gas_used = gas_used.saturating_add(result.tx_gas_used());
                    execution_gas_used =
                        execution_gas_used.saturating_add(result.gas().block_regular_gas_used());
                    state_gas_used =
                        state_gas_used.saturating_add(result.gas().block_state_gas_used());
                    hook.transaction_finished(TransactionFinished {
                        block_index,
                        total_blocks,
                        block_number: Some(header.number),
                        transaction_index,
                        total_transactions,
                        gas_used: result.tx_gas_used(),
                        execution_gas_used: result.gas().block_regular_gas_used(),
                        state_gas_used: result.gas().block_state_gas_used(),
                        success: result.is_success(),
                        logs: result.logs(),
                    });
                }

                assert_eq!(
                    block_gas_used(spec, gas_used, execution_gas_used, state_gas_used),
                    header.gas_used.saturating_to::<u64>(),
                    "revm block {block_number} gas must match its header",
                );

                revm_post_block(&mut evm, spec, block_withdrawals(block));

                let state = evm.finalize();
                evm.ctx_mut().db_mut().commit(state);
            }
            database.cache.block_hashes.insert(header.number, header.hash);

            parent_block_hash = Some(header.hash);
            if let Some(excess) = header.excess_blob_gas {
                parent_excess_blob_gas = excess.saturating_to::<u64>();
            }

            hook.block_finished(BlockFinished {
                block_index,
                total_blocks,
                block_number: Some(header.number),
                block_gas_used: Some(header.gas_used),
            });
        }
    }
}

struct ReplayRecorder {
    spec: SpecId,
    outcome: ReplayOutcome,
}

impl Hook for ReplayRecorder {
    fn block_started(&mut self, event: BlockStarted) {
        self.outcome.blocks.push(BlockOutcome {
            number: event.block_number.expect("replay block must have a number").saturating_to(),
            header_gas_used: event
                .block_gas_used
                .expect("replay block must have header gas")
                .saturating_to(),
            gas_used: 0,
            execution_gas_used: 0,
            state_gas_used: 0,
            block_gas_used: 0,
            txs: Vec::with_capacity(event.total_transactions),
        });
    }

    fn transaction_finished(&mut self, event: TransactionFinished<'_>) {
        let block = &mut self.outcome.blocks[event.block_index];
        block.gas_used = block.gas_used.saturating_add(event.gas_used);
        block.execution_gas_used =
            block.execution_gas_used.saturating_add(event.execution_gas_used);
        block.state_gas_used = block.state_gas_used.saturating_add(event.state_gas_used);
        block.txs.push(TxOutcome {
            gas_used: event.gas_used,
            success: event.success,
            logs: event.logs.len(),
            logs_digest: logs_digest(event.logs),
        });
    }

    fn block_finished(&mut self, event: BlockFinished) {
        let block = &mut self.outcome.blocks[event.block_index];
        block.block_gas_used = block_gas_used(
            self.spec,
            block.gas_used,
            block.execution_gas_used,
            block.state_gas_used,
        );
    }
}

// ---------------------------------------------------------------------------
// Fixture accessors, mirroring `crates/eest/src/blockchaintest/execute.rs`.
// ---------------------------------------------------------------------------

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

/// Checks that `case` is a canonical chain: every block carries a header, expects no
/// exception and extends the previous block, and `lastblockhash` names the final block.
fn assert_canonical(name: &str, case: &BlockchainTestCase) {
    assert!(!case.blocks.is_empty(), "replay fixture {name} has no blocks");
    let mut parent = &case.genesis_block_header;
    for (index, block) in case.blocks.iter().enumerate() {
        assert!(
            block.expect_exception.is_none(),
            "replay fixture {name} block {index} expects an exception; replay fixtures must only \
             contain valid blocks"
        );
        assert!(
            block.block_access_list.is_none()
                && block
                    .rlp_decoded
                    .as_ref()
                    .is_none_or(|decoded| decoded.block_access_list.is_none()),
            "replay fixture {name} block {index}: block access lists are not supported",
        );
        let header = block_header(block)
            .unwrap_or_else(|| panic!("replay fixture {name} block {index} has no header"));
        assert!(
            header.parent_hash == parent.hash && header.number == parent.number + U256::ONE,
            "replay fixture {name} block {index} does not extend the previous block"
        );
        parent = header;
    }
    assert!(
        case.lastblockhash == parent.hash,
        "replay fixture {name} lastblockhash does not match the final block"
    );
}

/// Digests a transaction's logs (address, topics and data, in emission order) so the
/// parity check covers log contents rather than only their count.
fn logs_digest(logs: &[Log]) -> B256 {
    let mut bytes = Vec::new();
    for log in logs {
        bytes.extend_from_slice(log.address.as_slice());
        bytes.extend_from_slice(&(log.data.topics().len() as u64).to_be_bytes());
        for topic in log.data.topics() {
            bytes.extend_from_slice(topic.as_slice());
        }
        bytes.extend_from_slice(&(log.data.data.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&log.data.data);
    }
    keccak256(bytes)
}

/// Returns the gas a block header must record, mirroring the EEST executor: cumulative
/// receipt gas before Amsterdam, `max(execution, state)` under EIP-8037 (Amsterdam+).
fn block_gas_used(
    spec: SpecId,
    gas_used: u64,
    execution_gas_used: u64,
    state_gas_used: u64,
) -> u64 {
    if spec.enables(SpecId::AMSTERDAM) { execution_gas_used.max(state_gas_used) } else { gas_used }
}

fn block_withdrawals(block: &Block) -> &[Withdrawal] {
    if let Some(withdrawals) = &block.withdrawals
        && !withdrawals.is_empty()
    {
        return withdrawals;
    }
    block.rlp_decoded.as_ref().map(|decoded| decoded.withdrawals.as_slice()).unwrap_or_default()
}

fn blob_params_for_timestamp(timestamp: U256, spec: SpecId) -> BlobParams {
    const MAINNET_BPO1_TIMESTAMP: u64 = 1_765_290_071;
    const MAINNET_BPO2_TIMESTAMP: u64 = 1_767_747_671;

    if spec.enables(SpecId::AMSTERDAM) || timestamp.saturating_to::<u64>() >= MAINNET_BPO2_TIMESTAMP
    {
        BlobParams::bpo2()
    } else if timestamp.saturating_to::<u64>() >= MAINNET_BPO1_TIMESTAMP {
        BlobParams::bpo1()
    } else if spec.enables(SpecId::OSAKA) {
        BlobParams::osaka()
    } else if spec.enables(SpecId::PRAGUE) {
        BlobParams::prague()
    } else {
        BlobParams::cancun()
    }
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
        other => panic!("replay fixture fork {other:?} is not supported"),
    }
}

fn revm_spec_id(spec: SpecId) -> RevmSpecId {
    match spec {
        SpecId::FRONTIER => RevmSpecId::FRONTIER,
        SpecId::HOMESTEAD => RevmSpecId::HOMESTEAD,
        SpecId::TANGERINE => RevmSpecId::TANGERINE,
        SpecId::SPURIOUS_DRAGON => RevmSpecId::SPURIOUS_DRAGON,
        SpecId::BYZANTIUM => RevmSpecId::BYZANTIUM,
        SpecId::PETERSBURG => RevmSpecId::PETERSBURG,
        SpecId::ISTANBUL => RevmSpecId::ISTANBUL,
        SpecId::BERLIN => RevmSpecId::BERLIN,
        SpecId::LONDON => RevmSpecId::LONDON,
        SpecId::MERGE => RevmSpecId::MERGE,
        SpecId::SHANGHAI => RevmSpecId::SHANGHAI,
        SpecId::CANCUN => RevmSpecId::CANCUN,
        SpecId::PRAGUE => RevmSpecId::PRAGUE,
        SpecId::OSAKA => RevmSpecId::OSAKA,
        SpecId::AMSTERDAM => RevmSpecId::AMSTERDAM,
        other => panic!("unsupported replay spec: {other:?}"),
    }
}

fn authorization_list(raw: &Transaction) -> Option<Vec<SignedAuthorization>> {
    let authorizations = raw.authorization_list.as_deref()?;
    Some(
        authorizations
            .iter()
            .map(|authorization| {
                serde_json::from_value(authorization.value.clone())
                    .expect("replay authorization must decode")
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// revm side.
// ---------------------------------------------------------------------------

type RevmEvm<'a> = revm::MainnetEvm<revm::handler::MainnetContext<&'a mut RevmInMemoryDB>>;

fn revm_pre_state(case: &BlockchainTestCase) -> RevmInMemoryDB {
    let mut database = CacheDB::new(EmptyDB::new());
    for (address, account) in &case.pre.0 {
        let code = RevmBytecode::new_raw_checked(account.code.clone())
            .unwrap_or_else(|_| RevmBytecode::new_legacy(account.code.clone()));
        let mut info = RevmAccountInfo {
            balance: account.balance,
            nonce: account.nonce.saturating_to::<u64>(),
            code: Some(code),
            ..Default::default()
        };
        // Mirrors evm2's `CacheDB::insert_account_info`: the bytecode lands in
        // the shared contract map and the account keeps only its code hash, so
        // execution has to resolve code through `code_by_hash`.
        database.insert_contract(&mut info);
        info.code = None;
        database.insert_account_info(*address, info);
        for (key, value) in &account.storage {
            database
                .insert_account_storage(*address, *key, *value)
                .expect("revm replay storage must insert");
        }
    }
    database
}

fn revm_seed_block_hashes(database: &mut RevmInMemoryDB, case: &BlockchainTestCase) {
    for block_hash in &case.block_hashes {
        database.cache.block_hashes.insert(block_hash.number, block_hash.hash);
    }
    database
        .cache
        .block_hashes
        .insert(case.genesis_block_header.number, case.genesis_block_header.hash);
}

fn revm_block_env(header: &BlockHeader, parent_excess_blob_gas: u64, spec: SpecId) -> RevmBlockEnv {
    let excess_blob_gas = header
        .excess_blob_gas
        .map(|gas| gas.saturating_to::<u64>())
        .unwrap_or(parent_excess_blob_gas);
    // The blob gas price is computed with the same `BlobParams` selection the
    // evm2 side uses, so both engines see an identical blob base fee.
    let blob_gasprice =
        blob_params_for_timestamp(header.timestamp, spec).calc_blob_fee(excess_blob_gas);
    RevmBlockEnv {
        number: header.number,
        beneficiary: header.coinbase,
        timestamp: header.timestamp,
        gas_limit: header.gas_limit.saturating_to::<u64>(),
        basefee: header.base_fee_per_gas.unwrap_or_default().saturating_to::<u64>(),
        difficulty: header.difficulty,
        prevrandao: header.difficulty.is_zero().then_some(header.mix_hash),
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice { excess_blob_gas, blob_gasprice }),
        slot_num: header.slot_number.unwrap_or_default().saturating_to::<u64>(),
    }
}

fn revm_pre_block(
    evm: &mut RevmEvm<'_>,
    spec: SpecId,
    block_number: u64,
    parent_block_hash: Option<B256>,
    header: &BlockHeader,
) {
    if block_number == 0 {
        return;
    }
    if spec.enables(SpecId::PRAGUE)
        && let Some(hash) = parent_block_hash
    {
        revm_system_call(evm, HISTORY_STORAGE_ADDRESS, hash.0.into(), "eip2935");
    }
    if spec.enables(SpecId::CANCUN)
        && let Some(root) = header.parent_beacon_block_root
    {
        revm_system_call(evm, BEACON_ROOTS_ADDRESS, root.0.into(), "eip4788");
    }
}

/// Mirrors the evm2 post-block transition: post-merge, so no block reward.
fn revm_post_block(evm: &mut RevmEvm<'_>, spec: SpecId, withdrawals: &[Withdrawal]) {
    assert!(spec.enables(SpecId::MERGE), "replay corpus must be post-merge");

    if spec.enables(SpecId::SHANGHAI) {
        for withdrawal in withdrawals {
            evm.ctx_mut()
                .journal_mut()
                .balance_incr(
                    withdrawal.address,
                    withdrawal.amount.saturating_mul(U256::from(ONE_GWEI)),
                )
                .expect("revm withdrawal credit must succeed");
        }
    }

    if spec.enables(SpecId::PRAGUE) {
        revm_system_call(evm, WITHDRAWAL_REQUEST_ADDRESS, Bytes::new(), "eip7002");
        revm_system_call(evm, CONSOLIDATION_REQUEST_ADDRESS, Bytes::new(), "eip7251");
    }

    if spec.enables(SpecId::AMSTERDAM) {
        revm_system_call(
            evm,
            evm2::evm::BUILDER_DEPOSIT_REQUEST_ADDRESS,
            Bytes::new(),
            "eip8282_deposit",
        );
        revm_system_call(
            evm,
            evm2::evm::BUILDER_EXIT_REQUEST_ADDRESS,
            Bytes::new(),
            "eip8282_exit",
        );
    }
}

fn revm_system_call(evm: &mut RevmEvm<'_>, address: Address, data: Bytes, label: &'static str) {
    let result = evm
        .system_call_one(address, data)
        .unwrap_or_else(|err| panic!("revm {label} system call must execute: {err:?}"));
    assert!(result.is_success(), "revm {label} system call must succeed");
}

fn revm_tx(raw: &Transaction) -> TxEnv {
    let caller = raw.sender.expect("replay transaction must carry a sender");
    let tx_type = raw.transaction_type.map(|ty| ty.saturating_to::<u8>()).unwrap_or(0);

    let mut builder = TxEnv::builder()
        .tx_type(Some(tx_type))
        .caller(caller)
        .gas_limit(raw.gas_limit.saturating_to::<u64>())
        .nonce(raw.nonce.saturating_to::<u64>())
        .value(raw.value)
        .data(raw.data.clone())
        .kind(raw.to.map_or(TxKind::Create, TxKind::Call))
        .chain_id(raw.chain_id.map(|id| id.saturating_to::<u64>()))
        .blob_hashes(raw.blob_versioned_hashes.clone())
        .max_fee_per_blob_gas(raw.max_fee_per_blob_gas.unwrap_or_default().saturating_to::<u128>())
        .authorization_list(
            authorization_list(raw).unwrap_or_default().into_iter().map(Either::Left).collect(),
        );
    if let Some(access_list) = revm_access_list(raw, tx_type) {
        builder = builder.access_list(access_list);
    }
    builder = if matches!(tx_type, 2..=4) {
        builder
            .gas_price(raw.max_fee_per_gas.unwrap_or_default().saturating_to::<u128>())
            .gas_priority_fee(Some(
                raw.max_priority_fee_per_gas.unwrap_or_default().saturating_to::<u128>(),
            ))
    } else {
        builder.gas_price(raw.gas_price.unwrap_or_default().saturating_to::<u128>())
    };

    builder.build().unwrap_or_else(|err| panic!("revm replay transaction must build: {err:?}"))
}

fn revm_access_list(raw: &Transaction, tx_type: u8) -> Option<RpcAccessList> {
    if tx_type == 0 {
        return None;
    }
    let Some(access_list) = &raw.access_list else {
        return (tx_type == 1).then(RpcAccessList::default);
    };
    Some(RpcAccessList(
        access_list
            .iter()
            .map(|item| RpcAccessListItem {
                address: item.address,
                storage_keys: item.storage_keys.clone(),
            })
            .collect(),
    ))
}
