//! cargo-nextest state test harness.

use evm2_statetest::{execute_str, state_test_root_from_env};
use std::path::Path;

fn root() -> String {
    state_test_root_from_env()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf())
        .display()
        .to_string()
}

fn pattern() -> &'static str {
    if state_test_root_from_env().is_some() { r"^.*\.json$" } else { r"^Cargo\.toml$" }
}

fn statetest(path: &Path, contents: String) -> datatest_stable::Result<()> {
    if state_test_root_from_env().is_none() {
        return Ok(());
    }
    execute_str(path, &contents)?;
    Ok(())
}

datatest_stable::harness! {
    { test = statetest, root = root(), pattern = pattern() },
}
