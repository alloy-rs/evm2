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
    "prague/eip6110_deposits",
    "prague/eip7002_el_triggerable_withdrawals",
    "prague/eip7251_consolidations",
    "prague/eip7685_general_purpose_el_requests",
];
