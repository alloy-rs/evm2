use super::{
    env::blockchain_test_roots,
    execute::{ExecuteConfig, ExecutionMode, execute_test_suite},
};
#[cfg(feature = "jit")]
use crate::{
    blockchaintest::execute::execute_test_suites, execution::CompiledMode, harness::compiled_roots,
};
use crate::{
    harness::{IgnoreList, TestSuite, run_json_harnesses},
    runner::{CREATE_COLLISION_IGNORED_TESTS, MAIN_CREATE_COLLISION_IGNORED_TESTS},
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
            ignored: ignored_tests(),
            run_file: run_file_jit,
            run_files: Some(run_files_jit),
        });
        suites.push(TestSuite {
            name: "blockchain_tests::aot",
            roots: compiled_roots(blockchain_test_roots(), CompiledMode::Aot),
            should_descend,
            ignored: ignored_tests(),
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
        ignored: ignored_tests(),
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

/// The ignore lists of every blockchain suite.
pub(crate) fn ignored_tests() -> Vec<IgnoreList> {
    vec![
        IgnoreList::new(IGNORED_TESTS),
        IgnoreList::main_only(CREATE_COLLISION_IGNORED_TESTS),
        IgnoreList::main_only(MAIN_CREATE_COLLISION_IGNORED_TESTS),
        IgnoreList::main_only(MAIN_IGNORED_TESTS),
    ]
}

#[rustfmt::skip]
const IGNORED_TESTS: &[&str] = &[
    // Block header and blob-gas validation is consensus-level block validation, not EVM execution.
    "cancun/eip4844_blobs/excess_blob_gas/invalid_blob_gas_used_in_header.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_excess_blob_gas_above_target_change.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_excess_blob_gas_change.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_excess_blob_gas_target_blobs_increase_from_zero.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_non_multiple_excess_blob_gas.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_static_excess_blob_gas.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_static_excess_blob_gas_from_zero_on_blobs_above_target.json",
    "cancun/eip4844_blobs/excess_blob_gas/invalid_zero_excess_blob_gas_in_header.json",
    "cancun/eip4844_blobs/excess_blob_gas_fork_transition/invalid_post_fork_block_without_blob_fields.json",
    "cancun/eip4844_blobs/excess_blob_gas_fork_transition/invalid_pre_fork_block_with_blob_fields.json",

    // These validate block RLP encoding or full blob sidecar rejection, which this runner does not decode/validate.
    "cancun/eip4844_blobs/blob_txs/invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/blob_txs_full/reject_valid_full_blob_in_block_rlp.json",

    // The harness does not validate Osaka block RLP size limits.
    "osaka/eip7934_block_rlp_limit/max_block_rlp_size/block_at_rlp_size_limit_boundary.json",
    "osaka/eip7934_block_rlp_limit/max_block_rlp_size/fork_transition_block_rlp_limit.json",

    // These validate block header fields/roots and belong to consensus-level block validation.
    // Amsterdam's `block_gas_limit_below_minimum` passes but shares this name.
    "frontier/validation/header/block_gas_limit_below_minimum.json",
    "london/validation/header/invalid_header.json",
    "shanghai/eip4895_withdrawals/withdrawals/withdrawals_root.json",

    // These check which header fields a block carries on either side of a fork, which is
    // consensus-level block validation.
    "eip7843_slotnum/fork_transition/invalid_post_fork_block_without_slot_number.json",
    "eip7843_slotnum/fork_transition/invalid_pre_fork_block_with_slot_number.json",
    "eip7928_block_level_access_lists/fork_transition/invalid_post_fork_block_without_bal_hash_field.json",
    "eip7928_block_level_access_lists/fork_transition/invalid_pre_fork_block_with_bal_hash_field.json",

    // These expect the block to be rejected for a request list that does not match the header, a
    // malformed deposit event layout, or an undeployed system contract. The harness neither
    // extracts EL requests nor validates the header's requests hash.
    "prague/eip6110_deposits/deposits/deposit_negative.json",
    "prague/eip6110_deposits/modified_contract/invalid_layout.json",
    "prague/eip6110_deposits/modified_contract/invalid_layout_with_swapped_decodable_offsets.json",
    "prague/eip6110_deposits/modified_contract/invalid_log_length.json",
    "prague/eip7002_el_triggerable_withdrawals/contract_deployment/system_contract_deployment.json",
    "prague/eip7002_el_triggerable_withdrawals/withdrawal_requests/withdrawal_requests_negative.json",
    "prague/eip7251_consolidations/consolidations/consolidation_requests_negative.json",
    "prague/eip7251_consolidations/contract_deployment/system_contract_deployment.json",
    "prague/eip7685_general_purpose_el_requests/multi_type_requests/invalid_multi_type_requests.json",
];

/// Fixtures from the `main` roots, which name the tests above `test_*` in flat directories.
#[rustfmt::skip]
const MAIN_IGNORED_TESTS: &[&str] = &[
    "cancun/eip4844_blobs/test_invalid_blob_gas_used_in_header.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_above_target_change.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_change.json",
    "cancun/eip4844_blobs/test_invalid_excess_blob_gas_target_blobs_increase_from_zero.json",
    "cancun/eip4844_blobs/test_invalid_non_multiple_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_post_fork_block_without_blob_fields.json",
    "cancun/eip4844_blobs/test_invalid_pre_fork_block_with_blob_fields.json",
    "cancun/eip4844_blobs/test_invalid_static_excess_blob_gas.json",
    "cancun/eip4844_blobs/test_invalid_static_excess_blob_gas_from_zero_on_blobs_above_target.json",
    "cancun/eip4844_blobs/test_invalid_zero_excess_blob_gas_in_header.json",
    "cancun/eip4844_blobs/test_invalid_blob_tx_contract_creation.json",
    "cancun/eip4844_blobs/test_reject_valid_full_blob_in_block_rlp.json",
    "osaka/eip7934_block_rlp_limit/test_block_at_rlp_size_limit_boundary.json",
    "osaka/eip7934_block_rlp_limit/test_fork_transition_block_rlp_limit.json",
    "prague/eip6110_deposits/test_deposit_negative.json",
    "prague/eip6110_deposits/test_invalid_layout.json",
    "prague/eip6110_deposits/test_invalid_log_length.json",
    "prague/eip7002_el_triggerable_withdrawals/test_system_contract_deployment.json",
    "prague/eip7002_el_triggerable_withdrawals/test_withdrawal_requests_negative.json",
    "prague/eip7251_consolidations/test_consolidation_requests_negative.json",
    "prague/eip7251_consolidations/test_system_contract_deployment.json",
    "prague/eip7685_general_purpose_el_requests/test_invalid_multi_type_requests.json",
];
