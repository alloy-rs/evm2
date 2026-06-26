use crate::{
    env::state_test_roots,
    execute::{ExecuteConfig, ExecutionMode, execute_test_suite},
    harness::{TestSuite, descend_all, run_json_harnesses},
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
            should_ignore,
            run_file: run_file_jit,
            run_files: Some(run_files_jit),
        });
        suites.push(TestSuite {
            name: "state-aot",
            roots: compiled_roots(state_test_roots(), CompiledMode::Aot),
            should_descend: descend_all,
            should_ignore,
            run_file: run_file_aot,
            run_files: None,
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
pub(crate) const IGNORED_TESTS: &[&str] = &[
    // Skip slow fixtures and create-collision fixtures that need storage-aware collision handling.
    "frontier/create/test_create_one_byte.json",
    "frontier/opcodes/test_all_opcodes.json",
    "frontier/opcodes/test_stack_overflow.json",
    "prague/eip2537_bls_12_381_precompiles/test_valid.json",
    "osaka/eip7939_count_leading_zeros/test_clz_opcode_scenarios.json",
    "static/state_tests/stBadOpcode/undefinedOpcodeFirstByte.json",
    "static/state_tests/stCreate2/CREATE2_FirstByte_loop.json",
    "static/state_tests/stEIP150singleCodeGasPrices/gasCost.json",
    "static/state_tests/stEIP150singleCodeGasPrices/gasCostBerlin.json",
    "static/state_tests/stPreCompiledContracts/precompsEIP2929Cancun.json",
    "static/state_tests/stTimeConsuming/",
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
