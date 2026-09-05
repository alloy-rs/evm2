//! Paired evm2/revm drivers for the mainnet block-replay benchmark.
//!
//! The benchmark's evm2 timing path stays on `evm2_eest`'s blockchain-test
//! executor (`crates/cli/benches/evm/mainnet.rs`). This module adds the missing
//! revm counterpart plus an evm2 mirror of the same block loop, so that the two
//! engines can be diffed transaction by transaction and the harness-side setup
//! cost can be measured on either side with the same code shape.
//!
//! The evm2 mirror follows `crates/eest/src/blockchaintest/execute.rs`
//! (`execute_case`/`execute_block`) step for step; the revm driver follows
//! `bins/revme/src/cmd/blockchaintest.rs` of the pinned revm 42 checkout, with
//! the block/commit cadence bent to match the evm2 side rather than revme's.

use alloy_consensus::{TypedTransaction, transaction::Recovered};
use alloy_eips::{eip7702::SignedAuthorization, eip7840::BlobParams};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, TxKind, U256};
use alloy_rpc_types_eth::{
    AccessList as RpcAccessList, AccessListItem as RpcAccessListItem, TransactionInput,
    TransactionRequest,
};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    env::{BlockEnv, BlockEnvExt},
    ethereum::{RecoveredTxEnvelope, TxEnvelope, ethereum_tx_registry},
    evm::{
        AccountChangeRef, AccountInfo, BEACON_ROOTS_ADDRESS, BlockStateAccumulator,
        CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS, InMemoryDB, StateChangeSink,
        StateChangeSource, SystemTx, Tee, WITHDRAWAL_REQUEST_ADDRESS,
    },
};
use evm2_eest::blockchaintest::{
    Block, BlockHeader, BlockchainTest, BlockchainTestCase, ForkSpec, Transaction, Withdrawal,
};
use revm::{
    Context, DatabaseCommit, ExecuteEvm, MainBuilder, MainContext, SystemCallEvm,
    context::{BlockEnv as RevmBlockEnv, CfgEnv, ContextTr, JournalTr, TxEnv},
    context_interface::{block::BlobExcessGasAndPrice, either::Either},
    database::{CacheDB, EmptyDB, InMemoryDB as RevmInMemoryDB},
    handler::EvmTr,
    primitives::hardfork::SpecId as RevmSpecId,
    state::{AccountInfo as RevmAccountInfo, Bytecode as RevmBytecode},
};
use std::{mem, path::Path};

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
}

/// Per-block outcome recorded identically by both engines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockOutcome {
    /// Block number taken from the fixture header.
    pub number: u64,
    /// `gasUsed` declared by the fixture header.
    pub header_gas_used: u64,
    /// Cumulative transaction gas used, as observed during execution.
    pub gas_used: u64,
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

    /// Returns the blocks whose observed gas used disagrees with the fixture header.
    pub fn header_gas_mismatches(&self) -> Vec<&BlockOutcome> {
        self.blocks.iter().filter(|block| block.gas_used != block.header_gas_used).collect()
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
        if left.gas_used != right.gas_used {
            mismatches.push(Mismatch {
                block,
                transaction: None,
                field: "block_gas_used",
                evm2: left.gas_used.to_string(),
                revm: right.gas_used.to_string(),
            });
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
        }
    }
    mismatches
}

/// A decoded blockchain-replay fixture holding exactly one test case.
#[derive(Debug)]
pub struct ReplayFixture {
    name: String,
    spec: SpecId,
    case: BlockchainTestCase,
}

impl ReplayFixture {
    /// Decodes the fixture at `path` and selects its single test case.
    ///
    /// # Panics
    ///
    /// Panics when the fixture cannot be read or does not contain exactly one case.
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
        let spec = fork_to_spec_id(case.network);
        Self { name, spec, case }
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
    pub const fn blocks(&self) -> usize {
        self.case.blocks.len()
    }

    /// Returns the total number of transactions across every block.
    pub fn transactions(&self) -> usize {
        self.case.blocks.iter().map(|block| block_transactions(block).len()).sum()
    }

    /// Builds the evm2 pre-state database and every block's transaction list.
    ///
    /// This is the harness-side work the timed replay repeats on each iteration;
    /// it is exposed so the benchmark can price it separately.
    pub fn setup_evm2(&self) -> (InMemoryDB, Vec<Vec<RecoveredTxEnvelope>>) {
        let mut database = evm2_pre_state(&self.case);
        evm2_seed_block_hashes(&mut database, &self.case);
        let txs = self
            .case
            .blocks
            .iter()
            .map(|block| block_transactions(block).iter().map(evm2_tx).collect())
            .collect();
        (database, txs)
    }

    /// Builds the revm pre-state database and every block's transaction list.
    pub fn setup_revm(&self) -> (RevmInMemoryDB, Vec<Vec<TxEnv>>) {
        let mut database = revm_pre_state(&self.case);
        revm_seed_block_hashes(&mut database, &self.case);
        let txs = self
            .case
            .blocks
            .iter()
            .map(|block| block_transactions(block).iter().map(revm_tx).collect())
            .collect();
        (database, txs)
    }

    /// Replays every block through evm2, mirroring the EEST blockchain executor.
    pub fn replay_evm2(&self) -> ReplayOutcome {
        let case = &self.case;
        let spec = self.spec;
        let mut database = evm2_pre_state(case);
        evm2_seed_block_hashes(&mut database, case);

        let mut parent_block_hash = Some(case.genesis_block_header.hash);
        let mut parent_excess_blob_gas =
            case.genesis_block_header.excess_blob_gas.unwrap_or_default().saturating_to::<u64>();
        let mut outcome = ReplayOutcome::default();

        for block in &case.blocks {
            let header = block_header(block).expect("replay fixture block must carry a header");
            let block_env = evm2_block_env(header, parent_excess_blob_gas, spec);

            let mut evm = Evm::<BaseEvmTypes>::new(
                spec,
                block_env,
                ethereum_tx_registry(spec),
                mem::take(&mut database),
                Precompiles::base(spec),
            );
            let mut block_state = BlockStateAccumulator::new();

            evm2_pre_block(&mut evm, &mut block_state, spec, block_env, parent_block_hash, header);

            let mut txs = Vec::with_capacity(block_transactions(block).len());
            let mut gas_used = 0u64;
            for raw in block_transactions(block) {
                let tx = evm2_tx(raw);
                let result = evm
                    .transact(&tx)
                    .unwrap_or_else(|err| panic!("evm2 replay transaction must execute: {err:?}"))
                    .commit_to(&mut block_state);
                gas_used = gas_used.saturating_add(result.tx_gas_used());
                txs.push(TxOutcome {
                    gas_used: result.tx_gas_used(),
                    success: result.status,
                    logs: result.logs.len(),
                });
            }

            evm2_post_block(&mut evm, &mut block_state, spec, block_withdrawals(block));

            let mut restored = mem::take(
                evm.database_as_mut::<InMemoryDB>().expect("block EVM database must be InMemoryDB"),
            );
            drop(evm);
            restored.commit_source(&block_state);
            restored.insert_block_hash(&header.number, &header.hash);
            database = restored;

            parent_block_hash = Some(header.hash);
            if let Some(excess) = header.excess_blob_gas {
                parent_excess_blob_gas = excess.saturating_to::<u64>();
            }

            outcome.blocks.push(BlockOutcome {
                number: header.number.saturating_to::<u64>(),
                header_gas_used: header.gas_used.saturating_to::<u64>(),
                gas_used,
                txs,
            });
        }

        outcome
    }

    /// Replays every block through revm using the same block sequence and cadence.
    pub fn replay_revm(&self) -> ReplayOutcome {
        let case = &self.case;
        let spec = self.spec;
        let revm_spec = revm_spec_id(spec);
        let mut database = revm_pre_state(case);
        revm_seed_block_hashes(&mut database, case);

        let mut cfg = CfgEnv::new();
        cfg.set_spec_and_mainnet_gas_params(revm_spec);

        let mut parent_block_hash = Some(case.genesis_block_header.hash);
        let mut parent_excess_blob_gas =
            case.genesis_block_header.excess_blob_gas.unwrap_or_default().saturating_to::<u64>();
        let mut outcome = ReplayOutcome::default();

        for block in &case.blocks {
            let header = block_header(block).expect("replay fixture block must carry a header");
            let block_env = revm_block_env(header, parent_excess_blob_gas, spec);
            let block_number = header.number.saturating_to::<u64>();

            let mut txs = Vec::with_capacity(block_transactions(block).len());
            let mut gas_used = 0u64;
            {
                let mut evm = Context::mainnet()
                    .with_cfg(cfg.clone())
                    .with_block(block_env)
                    .with_db(&mut database)
                    .build_mainnet();

                revm_pre_block(&mut evm, spec, block_number, parent_block_hash, header);

                for raw in block_transactions(block) {
                    let result = evm.transact_one(revm_tx(raw)).unwrap_or_else(|err| {
                        panic!("revm replay transaction must execute: {err:?}")
                    });
                    gas_used = gas_used.saturating_add(result.tx_gas_used());
                    txs.push(TxOutcome {
                        gas_used: result.tx_gas_used(),
                        success: result.is_success(),
                        logs: result.logs().len(),
                    });
                }

                revm_post_block(&mut evm, spec, block_withdrawals(block));

                let state = evm.finalize();
                evm.ctx_mut().db_mut().commit(state);
            }
            database.cache.block_hashes.insert(header.number, header.hash);

            parent_block_hash = Some(header.hash);
            if let Some(excess) = header.excess_blob_gas {
                parent_excess_blob_gas = excess.saturating_to::<u64>();
            }

            outcome.blocks.push(BlockOutcome {
                number: block_number,
                header_gas_used: header.gas_used.saturating_to::<u64>(),
                gas_used,
                txs,
            });
        }

        outcome
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

// ---------------------------------------------------------------------------
// evm2 side.
// ---------------------------------------------------------------------------

fn evm2_pre_state(case: &BlockchainTestCase) -> InMemoryDB {
    let mut database = InMemoryDB::default();
    for (address, account) in &case.pre.0 {
        let info = AccountInfo::default()
            .with_balance(account.balance)
            .with_nonce(account.nonce.saturating_to::<u64>())
            .with_code(
                Bytecode::new_raw_checked(account.code.clone())
                    .unwrap_or_else(|_| Bytecode::new_legacy(account.code.clone())),
            );
        database.insert_account_info(address, info);
        for (key, value) in &account.storage {
            database.insert_account_storage(address, key, value);
        }
    }
    database
}

fn evm2_seed_block_hashes(database: &mut InMemoryDB, case: &BlockchainTestCase) {
    for block_hash in &case.block_hashes {
        database.insert_block_hash(&block_hash.number, &block_hash.hash);
    }
    database.insert_block_hash(&case.genesis_block_header.number, &case.genesis_block_header.hash);
}

fn evm2_block_env(header: &BlockHeader, parent_excess_blob_gas: u64, spec: SpecId) -> BlockEnv {
    let excess_blob_gas = header
        .excess_blob_gas
        .map(|gas| gas.saturating_to::<u64>())
        .unwrap_or(parent_excess_blob_gas);
    BlockEnvExt {
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

fn evm2_pre_block(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    spec: SpecId,
    block: BlockEnv,
    parent_block_hash: Option<B256>,
    header: &BlockHeader,
) {
    if block.number.is_zero() {
        return;
    }
    if spec.enables(SpecId::PRAGUE)
        && let Some(hash) = parent_block_hash
    {
        evm2_system_call(
            evm,
            block_state,
            HISTORY_STORAGE_ADDRESS,
            Bytes::copy_from_slice(hash.as_slice()),
            "eip2935",
        );
    }
    if spec.enables(SpecId::CANCUN)
        && let Some(root) = header.parent_beacon_block_root
    {
        evm2_system_call(
            evm,
            block_state,
            BEACON_ROOTS_ADDRESS,
            Bytes::copy_from_slice(root.as_slice()),
            "eip4788",
        );
    }
}

/// Mirrors `post_block_transition`, minus the pre-merge block and ommer rewards:
/// the replay corpus is post-merge, so `block_reward` is always zero.
fn evm2_post_block(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    spec: SpecId,
    withdrawals: &[Withdrawal],
) {
    assert!(spec.enables(SpecId::MERGE), "replay corpus must be post-merge");

    if spec.enables(SpecId::SHANGHAI) {
        for withdrawal in withdrawals {
            evm2_increment_balance(
                evm,
                block_state,
                withdrawal.address,
                withdrawal.amount.saturating_mul(U256::from(ONE_GWEI)),
            );
        }
    }

    if spec.enables(SpecId::PRAGUE) {
        evm2_system_call(evm, block_state, WITHDRAWAL_REQUEST_ADDRESS, Bytes::new(), "eip7002");
        evm2_system_call(evm, block_state, CONSOLIDATION_REQUEST_ADDRESS, Bytes::new(), "eip7251");
    }

    if spec.enables(SpecId::AMSTERDAM) {
        evm2_system_call(
            evm,
            block_state,
            evm2::evm::BUILDER_DEPOSIT_REQUEST_ADDRESS,
            Bytes::new(),
            "eip8282_deposit",
        );
        evm2_system_call(
            evm,
            block_state,
            evm2::evm::BUILDER_EXIT_REQUEST_ADDRESS,
            Bytes::new(),
            "eip8282_exit",
        );
    }
}

fn evm2_system_call(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    address: Address,
    data: Bytes,
    label: &'static str,
) {
    let executed = evm
        .system_call(SystemTx::new(address, data))
        .unwrap_or_else(|err| panic!("evm2 {label} system call must execute: {err:?}"));
    assert!(executed.result().status, "evm2 {label} system call must succeed");
    let _ = executed.commit_to(block_state);
}

struct AccountStateChange {
    address: Address,
    original: Option<AccountInfo>,
    current: Option<AccountInfo>,
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

fn evm2_increment_balance(
    evm: &mut Evm<'_, BaseEvmTypes>,
    block_state: &mut BlockStateAccumulator,
    address: Address,
    amount: U256,
) {
    let original = evm
        .read_account_info(&address)
        .unwrap_or_else(|code| panic!("evm2 withdrawal account read must succeed: {code:?}"));
    let mut current = original.clone().unwrap_or_default();
    current.balance = current.balance.saturating_add(amount);
    if current.code_hash.is_zero() {
        current.code_hash = KECCAK256_EMPTY;
    }
    let current = (!current.is_empty()).then_some(current);

    let change = AccountStateChange { address, original, current };
    let mut sink = Tee::new(evm.overlay_db_mut(), block_state);
    let Ok(()) = change.visit(&mut sink);
}

fn evm2_tx(raw: &Transaction) -> RecoveredTxEnvelope {
    let caller = raw.sender.expect("replay transaction must carry a sender");
    let tx_type = raw.transaction_type.map(|ty| ty.saturating_to::<u8>()).unwrap_or(0);

    let mut request = TransactionRequest::default()
        .from(caller)
        .gas_limit(raw.gas_limit.saturating_to::<u64>())
        .nonce(raw.nonce.saturating_to::<u64>())
        .value(raw.value)
        .input(TransactionInput::from(raw.data.clone()));
    request.to = Some(raw.to.map_or(TxKind::Create, TxKind::Call));
    request.transaction_type = Some(tx_type);
    request.chain_id = raw.chain_id.map(|id| id.saturating_to::<u64>());
    if !matches!(tx_type, 2..=4) {
        request.gas_price = raw.gas_price.map(|price| price.saturating_to::<u128>());
        if request.gas_price.is_none()
            && (matches!(tx_type, 0 | 1)
                || (raw.max_fee_per_gas.is_none() && raw.max_priority_fee_per_gas.is_none()))
        {
            request.gas_price = Some(0);
        }
    }
    request.max_fee_per_gas = raw.max_fee_per_gas.map(|fee| fee.saturating_to::<u128>());
    request.max_priority_fee_per_gas =
        if raw.max_fee_per_gas.is_some() && raw.max_priority_fee_per_gas.is_none() {
            Some(0)
        } else {
            raw.max_priority_fee_per_gas.map(|fee| fee.saturating_to::<u128>())
        };
    request.max_fee_per_blob_gas = raw.max_fee_per_blob_gas.map(|fee| fee.saturating_to::<u128>());
    request.access_list = evm2_access_list(raw, tx_type);
    request.authorization_list = authorization_list(raw);
    if raw.max_fee_per_blob_gas.is_some() || tx_type == 3 || !raw.blob_versioned_hashes.is_empty() {
        request.blob_versioned_hashes = Some(raw.blob_versioned_hashes.clone());
    }

    let tx = request
        .build_consensus_tx()
        .unwrap_or_else(|err| panic!("replay transaction must build: {}", err.error));
    match tx {
        TypedTransaction::Legacy(tx) => Recovered::new_unchecked(TxEnvelope::Legacy(tx), caller),
        TypedTransaction::Eip2930(tx) => Recovered::new_unchecked(TxEnvelope::Eip2930(tx), caller),
        TypedTransaction::Eip1559(tx) => Recovered::new_unchecked(TxEnvelope::Eip1559(tx), caller),
        TypedTransaction::Eip4844(tx) => Recovered::new_unchecked(TxEnvelope::Eip4844(tx), caller),
        TypedTransaction::Eip7702(tx) => Recovered::new_unchecked(tx.into(), caller),
    }
}

fn evm2_access_list(raw: &Transaction, tx_type: u8) -> Option<RpcAccessList> {
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
