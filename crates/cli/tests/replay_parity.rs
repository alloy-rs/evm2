//! Differential check for the mainnet block-replay benchmark.
//!
//! Replays the benchmark corpus through evm2 and revm and compares every
//! transaction's gas used, success flag and logs, plus each block's EIP-8037 gas
//! split. The paired benchmark numbers are only meaningful while this passes.

use alloy_primitives::{B256, Log, U256};
use evm2_cli::replay_bench::ReplayFixture;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestHook, BlockchainTestNoopHook,
    BlockchainTestTransactionFinished, NameFilter,
    blockchaintest::{BlockchainTestCase, DecodedBlock, ForkSpec},
    execute_blockchain_tests_suite,
};
use std::path::{Path, PathBuf};

const FIXTURE: &str = "data/mainnet-25347446-25347455.bin.zst";

#[derive(Debug, PartialEq)]
struct RecordedTransaction {
    block_index: usize,
    transaction_index: usize,
    gas_used: u64,
    execution_gas_used: u64,
    state_gas_used: u64,
    success: bool,
    logs: Vec<Log>,
}

#[derive(Default)]
struct TransactionRecorder(Vec<RecordedTransaction>);

impl BlockchainTestHook for TransactionRecorder {
    fn transaction_finished(&mut self, event: BlockchainTestTransactionFinished<'_>) {
        self.0.push(RecordedTransaction {
            block_index: event.block_index,
            transaction_index: event.transaction_index,
            gas_used: event.gas_used,
            execution_gas_used: event.execution_gas_used,
            state_gas_used: event.state_gas_used,
            success: event.success,
            logs: event.logs.to_vec(),
        });
    }
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}

fn fixture_case() -> (String, BlockchainTestCase) {
    let suite =
        evm2_eest::read_blockchain_fixture(&workspace_path(FIXTURE)).expect("fixture must decode");
    suite.0.into_iter().next().expect("fixture must contain a case")
}

#[test]
fn mainnet_replay_matches_revm() {
    let path = workspace_path(FIXTURE);
    let fixture = ReplayFixture::load(&path);
    println!(
        "fixture {} | fork {:?} | blocks {} | transactions {}",
        fixture.name(),
        fixture.spec(),
        fixture.blocks(),
        fixture.transactions()
    );

    let evm2 = fixture.replay_evm2();
    let revm = fixture.replay_revm();

    println!(
        "{:>10} {:>6} {:>14} {:>14} {:>14}",
        "block", "txs", "header_gas", "evm2_gas", "revm_gas"
    );
    for (left, right) in evm2.blocks.iter().zip(&revm.blocks) {
        println!(
            "{:>10} {:>6} {:>14} {:>14} {:>14}",
            left.number,
            left.txs.len(),
            left.header_gas_used,
            left.block_gas_used,
            right.block_gas_used
        );
    }
    println!(
        "total: transactions evm2={} revm={} | gas evm2={} revm={}",
        evm2.transactions(),
        revm.transactions(),
        evm2.gas_used(),
        revm.gas_used()
    );

    // Both engines must reproduce every block header's `gasUsed` under the
    // fixture fork's rule; this is the same invariant the EEST executor enforces
    // on the benchmark's evm2 path.
    for (engine, outcome) in [("evm2", &evm2), ("revm", &revm)] {
        assert_eq!(outcome.blocks.len(), fixture.blocks(), "{engine} block count");
        assert_eq!(outcome.transactions(), fixture.transactions(), "{engine} transaction count");
        let mismatches = outcome.header_gas_mismatches();
        assert!(
            mismatches.is_empty(),
            "{engine} block gas disagrees with the fixture header: {mismatches:#?}"
        );
    }

    let mismatches = evm2_cli::replay_bench::diff(&evm2, &revm);
    println!("mismatches: {}", mismatches.len());
    for mismatch in &mismatches {
        println!("{mismatch:?}");
    }
    assert!(mismatches.is_empty(), "evm2 and revm disagree on {} entries", mismatches.len());
}

/// Receipt validation must leave transaction outcomes available to hooks.
#[test]
fn mainnet_replay_receipt_validation_preserves_hook_outcomes() {
    let path = workspace_path(FIXTURE);
    let fixture = ReplayFixture::load(&path);
    let suite = evm2_eest::read_blockchain_fixture(&path).expect("fixture must decode");
    let mut hook = TransactionRecorder::default();
    let summary = execute_blockchain_tests_suite(
        &path,
        &suite,
        BlockchainTestExecuteConfig { compare_receipt_root: true, ..Default::default() },
        &NameFilter::default(),
        &mut hook,
    )
    .expect("EEST replay must succeed");
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.skipped, 0);
    assert_eq!(hook.0.len(), fixture.transactions());
    assert!(hook.0.iter().any(|transaction| !transaction.logs.is_empty()));

    let mut revm_hook = TransactionRecorder::default();
    fixture.execute_revm(&mut revm_hook);
    assert_eq!(hook.0, revm_hook.0);
}

/// The replay loops commit every block; a fixture that expects an invalid block
/// must be rejected up front rather than replayed as if it were canonical.
#[test]
#[should_panic(expected = "expects an exception")]
fn replay_rejects_expected_invalid_blocks() {
    let (name, mut case) = fixture_case();
    case.blocks[0].expect_exception = Some("BlockException.INVALID_BLOCK".to_owned());
    let _fixture = ReplayFixture::from_case(name, case);
}

#[test]
#[should_panic(expected = "does not extend the previous block")]
fn replay_rejects_broken_parent_chain() {
    let (name, mut case) = fixture_case();
    let header = case.blocks[1].block_header.as_mut().expect("captured blocks carry headers");
    header.parent_hash = B256::ZERO;
    let _fixture = ReplayFixture::from_case(name, case);
}

#[test]
#[should_panic(expected = "replay corpus must be post-merge")]
fn replay_rejects_pre_merge_forks() {
    let (name, mut case) = fixture_case();
    case.network = ForkSpec::London;
    let _fixture = ReplayFixture::from_case(name, case);
}

#[test]
#[should_panic(expected = "block access lists are not supported")]
fn replay_rejects_block_access_lists() {
    let (name, mut case) = fixture_case();
    case.blocks[0].block_access_list = Some(Default::default());
    let _fixture = ReplayFixture::from_case(name, case);
}

#[test]
#[should_panic(expected = "block access lists are not supported")]
fn replay_rejects_decoded_block_access_lists() {
    let (name, mut case) = fixture_case();
    case.blocks[0].rlp_decoded =
        Some(DecodedBlock { block_access_list: Some(Default::default()), ..Default::default() });
    let _fixture = ReplayFixture::from_case(name, case);
}

#[test]
#[should_panic(expected = "gas must match its header")]
fn revm_replay_checks_header_gas_without_recording() {
    let (name, mut case) = fixture_case();
    let header = case.blocks[0].block_header.as_mut().expect("captured blocks carry headers");
    header.gas_used += U256::ONE;
    let fixture = ReplayFixture::from_case(name, case);
    fixture.execute_revm(&mut BlockchainTestNoopHook);
}
