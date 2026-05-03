//! cargo-nextest state test entrypoint.

use evm2_statetest::{RunConfig, find_json_tests, run_with_config, state_test_roots};
use std::{env, path::PathBuf, thread};

const DEFAULT_JOBS: usize = 28;
const JOBS_ENV: &str = "EVM2_STATETEST_JOBS";
const FAIL_FAST_ENV: &str = "EVM2_STATETEST_FAIL_FAST";
const NEXTEST_ENV: &str = "NEXTEST";
const NEXTEST_THREADS_ENV: &str = "NEXTEST_TEST_THREADS";
const SINGLE_THREAD_ENV: &str = "SINGLE_THREAD";

#[test]
fn statetest() {
    if env::var_os(NEXTEST_ENV).is_none() {
        eprintln!("Skipping state tests: run this target through cargo nextest.");
        return;
    }

    let roots = state_test_roots();
    if roots.is_empty() {
        eprintln!(
            "Skipping state tests: no fixtures found. Run ./scripts/setup-test-fixtures.sh or set EVM2_STATETEST_ROOT."
        );
        return;
    }

    for root in &roots {
        eprintln!("State tests: {} ({})", root.label, root.path.display());
    }

    let paths = roots.into_iter().map(|root| root.path).collect::<Vec<PathBuf>>();
    let files = find_json_tests(&paths).unwrap();
    run_with_config(
        files,
        RunConfig {
            jobs: Some(jobs()),
            single_thread: env::var_os(SINGLE_THREAD_ENV).is_some(),
            keep_going: env::var_os(FAIL_FAST_ENV).is_none(),
            ..RunConfig::default()
        },
    )
    .unwrap();
}

fn jobs() -> usize {
    env::var(JOBS_ENV)
        .or_else(|_| env::var(NEXTEST_THREADS_ENV))
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or_else(|| {
            thread::available_parallelism()
                .ok()
                .map(|jobs| jobs.get())
                .unwrap_or(1)
                .min(DEFAULT_JOBS)
        })
}
