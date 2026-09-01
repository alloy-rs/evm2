//! Differential check for the mainnet block-replay benchmark.
//!
//! Replays the benchmark corpus through evm2 and revm and compares every
//! transaction's gas used, success flag and log count. The paired benchmark
//! numbers are only meaningful while this passes.

use evm2_cli::replay_bench::ReplayFixture;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, NameFilter, execute_blockchain_tests_suite,
};
use std::path::{Path, PathBuf};

const FIXTURE: &str = "data/mainnet-25347446-25347455.bin.zst";

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
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
            left.gas_used,
            right.gas_used
        );
    }
    println!(
        "total: transactions evm2={} revm={} | gas evm2={} revm={}",
        evm2.transactions(),
        revm.transactions(),
        evm2.gas_used(),
        revm.gas_used()
    );

    // Both engines must reproduce every block header's `gasUsed`; this is the
    // same invariant the EEST executor enforces on the benchmark's evm2 path.
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
