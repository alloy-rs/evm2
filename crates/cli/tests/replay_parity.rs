//! Differential check for the mainnet block-replay benchmark.
//!
//! Replays the benchmark corpus through evm2 and revm and compares every
//! transaction's gas used, success flag and logs, plus each block's EIP-8037 gas
//! split. The paired benchmark numbers are only meaningful while this passes.

use alloy_primitives::B256;
use evm2_cli::replay_bench::ReplayFixture;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, NameFilter,
    blockchaintest::BlockchainTestCase, execute_blockchain_tests_suite,
};
use std::path::{Path, PathBuf};

const FIXTURE: &str = "data/mainnet-25347446-25347455.bin.zst";

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

/// Guards the mirror against drift from the benchmark's real evm2 path: the
/// EEST executor must accept the same fixture the mirror replays.
#[test]
fn mainnet_replay_eest_path_executes() {
    let path = workspace_path(FIXTURE);
    let suite = evm2_eest::read_blockchain_fixture(&path).expect("fixture must decode");
    let mut hook = BlockchainTestNoopHook;
    let summary = execute_blockchain_tests_suite(
        &path,
        &suite,
        BlockchainTestExecuteConfig::default(),
        &NameFilter::default(),
        &mut hook,
    )
    .expect("EEST replay must succeed");
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.skipped, 0);
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
