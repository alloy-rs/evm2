use super::{
    env::blockchain_test_roots,
    execute::{ExecuteConfig, execute_test_suite},
};
use crate::harness::{TestSuite, run_json_harness};
use libtest_mimic::Failed;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

/// Runs the cargo-nextest blockchain test harness.
pub fn run() -> ExitCode {
    run_json_harness("blockchain", blockchain_test_roots(), should_descend, should_ignore, run_file)
}

pub(crate) fn suite() -> TestSuite {
    TestSuite {
        name: "blockchain",
        roots: blockchain_test_roots(),
        should_descend,
        should_ignore,
        run_file,
    }
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    execute_test_suite(&path, ExecuteConfig::default())
        .map(|_| ())
        .map_err(|err| err.to_string().into())
}

fn should_descend(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    !matches!(
        name,
        "blockchain_tests_engine" | "blockchain_tests_engine_x" | "blockchain_tests_sync"
    )
}

fn should_ignore(name: &str) -> bool {
    IGNORED_TESTS.iter().any(|ignored| name.contains(ignored))
}

#[rustfmt::skip]
const IGNORED_TESTS: &[&str] = &[
    "cancun/eip4844_blobs/test_blob_type_tx_pre_fork.json",
    "cancun/eip4844_blobs/test_invalid_blob_gas_used_in_header.json",
    "cancun/eip4844_blobs/test_invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/test_invalid_block_blob_count.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_above_target_change.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_change.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_target_blobs_increase_from_zero.json",
    "cancun/eip4844_blobs/test_invalid_negative_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_non_multiple_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_post_fork_block_without_blob_fields.json",
    "cancun/eip4844_blobs/test_invalid_pre_fork_block_with_blob_fields.json",
    "cancun/eip4844_blobs/test_invalid_static_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_static_excess_blob_gas_from_zero_on_blobs_above_target.json",
    "cancun/eip4844_blobs/test_invalid_tx_blob_count.json",
    "cancun/eip4844_blobs/test_invalid_zero_excess_blob_gas_in_header.json",
    "cancun/eip4844_blobs/test_reject_valid_full_blob_in_block_rlp.json",
    "osaka/eip7594_peerdas/test_invalid_max_blobs_per_tx.json",
    "osaka/eip7594_peerdas/test_max_blobs_per_tx_fork_transition.json",
    "osaka/eip7825_transaction_gas_limit_cap/test_tx_gas_larger_than_block_gas_limit.json",
    "osaka/eip7934_block_rlp_limit/test_block_at_rlp_size_limit_boundary.json",
    "osaka/eip7934_block_rlp_limit/test_fork_transition_block_rlp_limit.json",
    "prague/eip6110_deposits",
    "prague/eip7002_el_triggerable_withdrawals",
    "prague/eip7251_consolidations",
    "prague/eip7685_general_purpose_el_requests",
    "static/state_tests/stEIP1559/lowGasLimit.json",
];
