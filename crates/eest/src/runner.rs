use crate::{
    env::state_test_roots,
    execute::{ExecuteConfig, ExecutionMode, execute_test_suite},
    harness::{IgnoreList, TestSuite, descend_all, run_json_harnesses},
};
#[cfg(feature = "jit")]
use crate::{execute::execute_test_suites, execution::CompiledMode, harness::compiled_roots};
use libtest_mimic::Failed;
use std::{path::PathBuf, process::ExitCode};

/// Runs the cargo-nextest state test harness.
pub fn run() -> ExitCode {
    run_json_harnesses(suites())
}

pub(crate) fn suites() -> Vec<TestSuite> {
    #[cfg(feature = "jit")]
    {
        let mut suites = vec![suite()];
        suites.push(TestSuite {
            name: "statetests::jit",
            roots: compiled_roots(state_test_roots(), CompiledMode::Jit),
            should_descend: descend_all,
            ignored: ignored_tests(),
            run_file: run_file_jit,
            run_files: Some(run_files_jit),
        });
        suites.push(TestSuite {
            name: "statetests::aot",
            roots: compiled_roots(state_test_roots(), CompiledMode::Aot),
            should_descend: descend_all,
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

fn suite() -> TestSuite {
    TestSuite {
        name: "state",
        roots: state_test_roots(),
        should_descend: descend_all,
        ignored: ignored_tests(),
        run_file,
        run_files: None,
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
    execute_test_suites(&paths, ExecuteConfig { mode: mode.execution_mode(), ..Default::default() })
        .map(|_| ())
        .map_err(|err| err.to_string().into())
}

#[rustfmt::skip]
const IGNORED_TESTS: &[&str] = &[
    // Each fork runs 2^32 - 1 Blake2f rounds: about 275s of CPU in the interpreter.
    "CALLBlake2f_MaxRounds.json",

    // The state-test runner takes the sender from the fixture and does not recover or
    // validate legacy-tx signatures, so v/r/s and chain-id rejection tests cannot pass.
    "frontier/validation/transaction/bad_v_r_s.json",
    "frontier/validation/transaction/unrecoverable_signature.json",
    "london/eip1559_fee_market_change/tx_type/invalid_chain_id.json",
];

/// Create-collision fixtures. Core collides on nonce and code only: it does not implement the
/// EIP-7610 check that the target's storage is empty.
#[rustfmt::skip]
pub(crate) const CREATE_COLLISION_IGNORED_TESTS: &[&str] = &[
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
];

/// EIP-7610 create-collision fixtures from the `main` roots, ignored for the same reason as
/// [`CREATE_COLLISION_IGNORED_TESTS`].
pub(crate) const MAIN_CREATE_COLLISION_IGNORED_TESTS: &[&str] = &["eip7610_create_collision"];

/// The ignore lists of every state suite.
pub(crate) fn ignored_tests() -> Vec<IgnoreList> {
    vec![
        IgnoreList::new(IGNORED_TESTS),
        IgnoreList::new(CREATE_COLLISION_IGNORED_TESTS),
        IgnoreList::main_only(MAIN_CREATE_COLLISION_IGNORED_TESTS),
    ]
}
