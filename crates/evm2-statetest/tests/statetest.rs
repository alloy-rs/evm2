//! cargo-nextest state test harness.

use evm2_statetest::{DEFAULT_STATE_TEST_ROOT, execute_str};
use std::{
    env,
    path::{Path, PathBuf},
};

fn root() -> String {
    let mut root = env::var_os("EVM2_STATETEST_ROOT")
        .or_else(|| env::var_os("ETHEREUM_TESTS"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_TEST_ROOT));
    if let Some(subdir) = env::var_os("SUBDIR")
        && !subdir.is_empty()
    {
        root.push(subdir);
    }
    root.display().to_string()
}

fn statetest(path: &Path, contents: String) -> datatest_stable::Result<()> {
    execute_str(path, &contents)?;
    Ok(())
}

datatest_stable::harness! {
    { test = statetest, root = root(), pattern = r"^.*\.json$" },
}
