use crate::{
    env::state_test_roots,
    execute::{ExecuteConfig, execute_test_suite},
    harness::{TestSuite, descend_all, run_json_harness},
};
use libtest_mimic::Failed;
use std::{path::PathBuf, process::ExitCode};

/// Runs the cargo-nextest state test harness.
pub fn run() -> ExitCode {
    run_json_harness("state", state_test_roots(), descend_all, should_ignore, run_file)
}

pub(crate) fn suite() -> TestSuite {
    TestSuite {
        name: "state",
        roots: state_test_roots(),
        should_descend: descend_all,
        should_ignore,
        run_file,
    }
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    execute_test_suite(&path, ExecuteConfig::default())
        .map(|_| ())
        .map_err(|err| err.to_string().into())
}

#[rustfmt::skip]
const IGNORED_TESTS: &[&str] = &[
    // Skip slow fixtures and create-collision fixtures that need storage-aware collision handling.
    "stTimeConsuming/static_Call50000_sha256.json",
    "CALLBlake2f_MaxRounds.json",
    "loopExp",
    "loopMul.json",
    "stQuadraticComplexityTest/Call1MB1024Calldepth.json",
    "stQuadraticComplexityTest/Create1000",
    "stRecursiveCreate/recursiveCreate",
    "stRevertTest/LoopCallsDepthThenRevert",
    "stRevertTest/LoopDelegateCallsDepthThenRevert",
    "stSolidityTest/RecursiveCreateContracts",
    "stStaticCall/static_Call1MB1024Calldepth.json",
    "stStaticCall/static_Call50000_sha256",
    "stStaticCall/static_CallRecursiveBomb",
    "stStaticCall/static_LoopCallsDepthThenRevert",
    "stSystemOperationsTest/CallRecursiveBomb",

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
];

fn should_ignore(name: &str) -> bool {
    IGNORED_TESTS.iter().any(|pattern| name.contains(pattern))
}
