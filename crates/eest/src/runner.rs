#[cfg(feature = "jit")]
use crate::harness::{COMPILED_FIXTURE_STACK_SIZE, TestRoot, run_with_stack};
use crate::{
    env::state_test_roots,
    execute::{ExecuteConfig, ExecutionMode, execute_test_suite},
    harness::{TestSuite, descend_all, run_json_harnesses},
};
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
            name: "state-jit",
            roots: mode_roots(state_test_roots(), ModeName::Jit),
            should_descend: descend_all,
            should_ignore,
            run_file: run_file_jit,
        });
        suites.push(TestSuite {
            name: "state-aot",
            roots: mode_roots(state_test_roots(), ModeName::Aot),
            should_descend: descend_all,
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

fn suite() -> TestSuite {
    TestSuite {
        name: "state",
        roots: state_test_roots(),
        should_descend: descend_all,
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
    run_compiled_file(path, ExecutionMode::Jit)
}

#[cfg(feature = "jit")]
fn run_file_aot(path: PathBuf) -> Result<(), Failed> {
    run_compiled_file(path, ExecutionMode::Aot)
}

#[cfg(feature = "jit")]
fn run_compiled_file(path: PathBuf, mode: ExecutionMode) -> Result<(), Failed> {
    run_with_stack("eest-state-compiled", COMPILED_FIXTURE_STACK_SIZE, move || {
        run_file_with_mode(path, mode)
    })
}

#[cfg(feature = "jit")]
#[derive(Clone, Copy, Debug)]
enum ModeName {
    Jit,
    Aot,
}

#[cfg(feature = "jit")]
fn mode_roots(roots: Vec<TestRoot>, mode: ModeName) -> Vec<TestRoot> {
    roots
        .into_iter()
        .map(|root| TestRoot { name: mode_root_name(root.name, mode), ..root })
        .collect()
}

#[cfg(feature = "jit")]
fn mode_root_name(name: &'static str, mode: ModeName) -> &'static str {
    match (name, mode) {
        ("statetests", ModeName::Jit) => "statetests::jit",
        ("statetests", ModeName::Aot) => "statetests::aot",
        ("statetests::custom", ModeName::Jit) => "statetests::custom::jit",
        ("statetests::custom", ModeName::Aot) => "statetests::custom::aot",
        ("statetests::devnet", ModeName::Jit) => "statetests::devnet::jit",
        ("statetests::devnet", ModeName::Aot) => "statetests::devnet::aot",
        ("legacy::cancun", ModeName::Jit) => "legacy::cancun::jit",
        ("legacy::cancun", ModeName::Aot) => "legacy::cancun::aot",
        ("legacy::constantinople", ModeName::Jit) => "legacy::constantinople::jit",
        ("legacy::constantinople", ModeName::Aot) => "legacy::constantinople::aot",
        ("legacy::ethereum_tests", ModeName::Jit) => "legacy::ethereum_tests::jit",
        ("legacy::ethereum_tests", ModeName::Aot) => "legacy::ethereum_tests::aot",
        _ => name,
    }
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

#[cfg(all(test, feature = "jit"))]
mod tests {
    use super::*;

    #[test]
    fn mode_root_name_maps_state_roots() {
        assert_eq!(mode_root_name("statetests", ModeName::Jit), "statetests::jit");
        assert_eq!(mode_root_name("statetests", ModeName::Aot), "statetests::aot");
        assert_eq!(mode_root_name("statetests::custom", ModeName::Jit), "statetests::custom::jit");
        assert_eq!(mode_root_name("statetests::custom", ModeName::Aot), "statetests::custom::aot");
        assert_eq!(mode_root_name("statetests::devnet", ModeName::Jit), "statetests::devnet::jit");
        assert_eq!(mode_root_name("statetests::devnet", ModeName::Aot), "statetests::devnet::aot");
    }

    #[test]
    fn mode_root_name_maps_legacy_roots() {
        assert_eq!(mode_root_name("legacy::cancun", ModeName::Jit), "legacy::cancun::jit");
        assert_eq!(mode_root_name("legacy::cancun", ModeName::Aot), "legacy::cancun::aot");
        assert_eq!(
            mode_root_name("legacy::constantinople", ModeName::Jit),
            "legacy::constantinople::jit"
        );
        assert_eq!(
            mode_root_name("legacy::constantinople", ModeName::Aot),
            "legacy::constantinople::aot"
        );
        assert_eq!(
            mode_root_name("legacy::ethereum_tests", ModeName::Jit),
            "legacy::ethereum_tests::jit"
        );
        assert_eq!(
            mode_root_name("legacy::ethereum_tests", ModeName::Aot),
            "legacy::ethereum_tests::aot"
        );
    }
}
