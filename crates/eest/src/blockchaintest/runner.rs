use super::{
    env::blockchain_test_roots,
    execute::{ExecuteConfig, ExecutionMode, execute_test_suite},
};
use crate::harness::{TestSuite, run_json_harnesses};
#[cfg(feature = "jit")]
use crate::{
    execution::CompiledMode,
    harness::{COMPILED_FIXTURE_STACK_SIZE, compiled_roots, run_with_stack},
};
use libtest_mimic::Failed;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

/// Runs the cargo-nextest blockchain test harness.
pub fn run() -> ExitCode {
    run_json_harnesses(suites())
}

pub(crate) fn suites() -> Vec<TestSuite> {
    #[cfg(feature = "jit")]
    {
        let mut suites = vec![suite()];
        suites.push(TestSuite {
            name: "blockchain-jit",
            roots: compiled_roots(blockchain_test_roots(), CompiledMode::Jit),
            should_descend,
            should_ignore,
            run_file: run_file_jit,
        });
        suites.push(TestSuite {
            name: "blockchain-aot",
            roots: compiled_roots(blockchain_test_roots(), CompiledMode::Aot),
            should_descend,
            should_ignore,
            run_file: run_file_aot,
        });
        suites
    }

    #[cfg(not(feature = "jit"))]
    {
        vec![suite()]
    }
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
    run_file_with_mode(path, ExecutionMode::Interpreter)
}

fn run_file_with_mode(path: PathBuf, mode: ExecutionMode) -> Result<(), Failed> {
    execute_test_suite(&path, ExecuteConfig { mode, ..Default::default() })
        .map(|_| ())
        .map_err(|err| err.to_string().into())
}

#[cfg(feature = "jit")]
fn run_file_jit(path: PathBuf) -> Result<(), Failed> {
    run_compiled_file(path, CompiledMode::Jit)
}

#[cfg(feature = "jit")]
fn run_file_aot(path: PathBuf) -> Result<(), Failed> {
    run_compiled_file(path, CompiledMode::Aot)
}

#[cfg(feature = "jit")]
fn run_compiled_file(path: PathBuf, mode: CompiledMode) -> Result<(), Failed> {
    run_with_stack("eest-blockchain-compiled", COMPILED_FIXTURE_STACK_SIZE, move || {
        run_file_with_mode(path, mode.execution_mode())
    })
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
    // Block header/blob-gas validation is consensus-level block validation, not EVM execution.
    "cancun/eip4844_blobs/test_blob_type_tx_pre_fork.json",
    "cancun/eip4844_blobs/test_invalid_blob_gas_used_in_header.json",
    "cancun/eip4844_blobs/test_invalid_block_blob_count.json",
    // The same tests were renamed into subdirectories on BAL releases.
    "cancun/eip4844_blobs/blob_txs/invalid_block_blob_count.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_blob_gas_used_in_header.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_above_target_change.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_change.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_target_blobs_increase_from_zero.json",
    "cancun/eip4844_blobs/test_invalid_negative_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_non_multiple_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_post_fork_block_without_blob_fields.json",
    "cancun/eip4844_blobs/test_invalid_pre_fork_block_with_blob_fields.json",
    "cancun/eip4844_blobs/blob_txs/blob_type_tx_pre_fork.json",
    "cancun/eip4844_blobs/excess_blob_gas_fork_transition/invalid_post_fork_block_without_blob_fields.json",
    "cancun/eip4844_blobs/excess_blob_gas_fork_transition/invalid_pre_fork_block_with_blob_fields.json",
    "cancun/eip4844_blobs/test_invalid_static_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_static_excess_blob_gas_from_zero_on_blobs_above_target.json",
    "cancun/eip4844_blobs/test_invalid_zero_excess_blob_gas_in_header.json",
    // The same excess-blob-gas tests were renamed into this subdirectory on BAL releases.
    "cancun/eip4844_blobs/excess_blob_gas/",

    // These validate block RLP encoding or full blob sidecar rejection, which this runner does not decode/validate.
    "cancun/eip4844_blobs/test_invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/test_reject_valid_full_blob_in_block_rlp.json",
    // The same tests were renamed into subdirectories on BAL releases.
    "cancun/eip4844_blobs/blob_txs/invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/blob_txs_full/reject_valid_full_blob_in_block_rlp.json",

    // These are block-level blob count / fork transition tests, not transaction execution tests.
    "cancun/eip4844_blobs/test_invalid_tx_blob_count.json",
    "osaka/eip7594_peerdas/test_invalid_max_blobs_per_tx.json",
    "osaka/eip7594_peerdas/test_max_blobs_per_tx_fork_transition.json",
    // The same tests were renamed into subdirectories on BAL releases.
    "cancun/eip4844_blobs/blob_txs/invalid_tx_blob_count.json",
    "osaka/eip7594_peerdas/max_blob_per_tx/invalid_max_blobs_per_tx.json",
    "osaka/eip7594_peerdas/max_blob_per_tx/max_blobs_per_tx_fork_transition.json",

    // Create-collision fixtures need storage-aware collision handling.
    "eip7610_create_collision",
    "InitCollision.json",
    "InitCollisionParis.json",
    "RevertInCreateInInit.json",
    "RevertInCreateInInit_Paris.json",
    "RevertInCreateInInitCreate2.json",
    "RevertInCreateInInitCreate2Paris.json",
    "create2collisionStorage.json",
    "create2collisionStorageParis.json",
    "dynamicAccountOverwriteEmpty.json",
    "dynamicAccountOverwriteEmpty_Paris.json",

    // The harness does not track cumulative block gas allowance or validate Osaka block RLP size limits.
    "osaka/eip7825_transaction_gas_limit_cap/test_tx_gas_larger_than_block_gas_limit.json",
    "osaka/eip7934_block_rlp_limit/test_block_at_rlp_size_limit_boundary.json",
    "osaka/eip7934_block_rlp_limit/test_fork_transition_block_rlp_limit.json",
    // The same tests were renamed into subdirectories on BAL releases.
    "osaka/eip7825_transaction_gas_limit_cap/tx_gas_limit/tx_gas_larger_than_block_gas_limit.json",
    "osaka/eip7934_block_rlp_limit/max_block_rlp_size/block_at_rlp_size_limit_boundary.json",
    "osaka/eip7934_block_rlp_limit/max_block_rlp_size/fork_transition_block_rlp_limit.json",

    // These validate block header fields/roots and belong to consensus-level block validation.
    "frontier/validation/header/gas_limit_below_minimum.json",
    "london/validation/header/invalid_header.json",
    "shanghai/eip4895_withdrawals/withdrawals/withdrawals_root.json",

    // These validate block access list format/content/hash and belong to consensus-level block validation.
    "amsterdam/eip7928_block_level_access_lists/block_access_lists_invalid/",

    // These validate block/transaction gas allowance rules, not EVM execution.
    "amsterdam/eip8037_state_creation_gas_cost_increase/block_2d_gas_accounting/tx_rejected_when_regular_gas_exceeds_block_limit_small.json",
    "amsterdam/eip8037_state_creation_gas_cost_increase/state_gas_reservoir/creation_tx_state_check_exceeded.json",

    // Prague request/deposit fixtures validate EL request extraction and system-contract block processing.
    "prague/eip6110_deposits",
    "prague/eip7002_el_triggerable_withdrawals",
    "prague/eip7251_consolidations",
    "prague/eip7685_general_purpose_el_requests",

    // This fixture has a block gas limit below the transaction intrinsic gas and belongs to block validation.
    "static/state_tests/stEIP1559/lowGasLimit.json",

    // This bundled Frontier scenarios file exceeds the compiled-backend per-test budget.
    "blockchain_tests::jit::frontier/scenarios/test_scenarios.json",
    "blockchain_tests::aot::frontier/scenarios/test_scenarios.json",
];

#[cfg(all(test, feature = "jit"))]
mod tests {
    use super::*;

    #[test]
    fn scenarios_ignore_is_compiled_backend_only() {
        assert!(!should_ignore("blockchain_tests::frontier/scenarios/test_scenarios.json"));
        assert!(should_ignore("blockchain_tests::jit::frontier/scenarios/test_scenarios.json"));
        assert!(should_ignore("blockchain_tests::aot::frontier/scenarios/test_scenarios.json"));
    }
}
