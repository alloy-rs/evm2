use super::{
    env::blockchain_test_roots,
    execute::{ExecuteConfig, ExecutionMode, execute_test_suite},
};
use crate::harness::{TestSuite, run_json_harnesses};
#[cfg(feature = "jit")]
use crate::{
    blockchaintest::execute::execute_test_suites, execution::CompiledMode, harness::compiled_roots,
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
            name: "blockchain_tests::jit",
            roots: compiled_roots(blockchain_test_roots(), CompiledMode::Jit),
            should_descend,
            should_ignore,
            run_file: run_file_jit,
            run_files: Some(run_files_jit),
        });
        suites.push(TestSuite {
            name: "blockchain_tests::aot",
            roots: compiled_roots(blockchain_test_roots(), CompiledMode::Aot),
            should_descend,
            should_ignore,
            run_file: run_file_aot,
            run_files: Some(run_files_aot),
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
        run_files: None,
    }
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    run_file_with_mode(path, ExecutionMode::Interpreter)
}

fn run_file_with_mode(path: PathBuf, mode: ExecutionMode) -> Result<(), Failed> {
    execute_test_suite(
        &path,
        ExecuteConfig {
            mode,
            compare_state_root: true,
            compare_receipt_root: true,
            ..Default::default()
        },
    )
    .map(|_| ())
    .map_err(|err| err.to_string().into())
}

#[cfg(feature = "jit")]
fn run_file_jit(path: PathBuf) -> Result<(), Failed> {
    run_compiled_file(path, CompiledMode::Jit)
}

#[cfg(feature = "jit")]
fn run_files_jit(paths: Vec<PathBuf>) -> Result<(), Failed> {
    run_compiled_files(paths, CompiledMode::Jit)
}

#[cfg(feature = "jit")]
fn run_file_aot(path: PathBuf) -> Result<(), Failed> {
    run_compiled_file(path, CompiledMode::Aot)
}

#[cfg(feature = "jit")]
fn run_files_aot(paths: Vec<PathBuf>) -> Result<(), Failed> {
    run_compiled_files(paths, CompiledMode::Aot)
}

#[cfg(feature = "jit")]
fn run_compiled_file(path: PathBuf, mode: CompiledMode) -> Result<(), Failed> {
    run_file_with_mode(path, mode.execution_mode())
}

#[cfg(feature = "jit")]
fn run_compiled_files(paths: Vec<PathBuf>, mode: CompiledMode) -> Result<(), Failed> {
    execute_test_suites(
        &paths,
        ExecuteConfig {
            mode: mode.execution_mode(),
            compare_state_root: true,
            compare_receipt_root: true,
            ..Default::default()
        },
    )
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
pub(crate) const IGNORED_TESTS: &[&str] = &[
    // These validate block RLP structure or full blob sidecar rejection, which this runner does not decode/validate.
    "cancun/eip4844_blobs/test_invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/test_reject_valid_full_blob_in_block_rlp.json",
    // The same tests were renamed into subdirectories on BAL releases.
    "cancun/eip4844_blobs/blob_txs/invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/blob_txs_full/reject_valid_full_blob_in_block_rlp.json",

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

    // These validate block header fields/roots and belong to consensus-level block validation.
    "frontier/validation/header/gas_limit_below_minimum.json",
    // The same test was renamed with a `block_` prefix on glamsterdam-devnet releases.
    "frontier/validation/header/block_gas_limit_below_minimum.json",
    "london/validation/header/invalid_header.json",
    "shanghai/eip4895_withdrawals/withdrawals/withdrawals_root.json",
    // The same tests under the EEST main suite's flat `test_*` naming.
    "frontier/validation/test_gas_limit_below_minimum.json",
    "london/validation/test_invalid_header.json",
    "shanghai/eip4895_withdrawals/test_withdrawals_root.json",

    // These check which header fields a block carries on either side of a fork, which is
    // consensus-level block validation.
    "eip7843_slotnum/fork_transition/invalid_post_fork_block_without_slot_number.json",
    "eip7843_slotnum/fork_transition/invalid_pre_fork_block_with_slot_number.json",
    "eip7928_block_level_access_lists/fork_transition/invalid_post_fork_block_without_bal_hash_field.json",
    "eip7928_block_level_access_lists/fork_transition/invalid_pre_fork_block_with_bal_hash_field.json",

    // Prague request/deposit fixtures validate EL request extraction and system-contract block processing.
    "prague/eip6110_deposits",
    "prague/eip7002_el_triggerable_withdrawals",
    "prague/eip7251_consolidations",
    "prague/eip7685_general_purpose_el_requests",

    // This fixture has a block gas limit below the transaction intrinsic gas and belongs to block validation.
    "static/state_tests/stEIP1559/lowGasLimit.json",
];
