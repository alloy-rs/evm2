use std::{env, path::PathBuf};

/// Environment variable for the state test root.
pub const STATE_TEST_ROOT_ENV: &str = "EVM2_STATETEST_ROOT";

/// Fallback environment variable for the state test root.
pub const ETHEREUM_TESTS_ENV: &str = "ETHEREUM_TESTS";

/// Optional environment variable for selecting a subdirectory under the test root.
pub const STATE_TEST_SUBDIR_ENV: &str = "SUBDIR";

/// Returns the state-test root configured through environment variables.
pub fn state_test_root_from_env() -> Option<PathBuf> {
    let mut root = env::var_os(STATE_TEST_ROOT_ENV)
        .or_else(|| env::var_os(ETHEREUM_TESTS_ENV))
        .map(PathBuf::from)?;
    if let Some(subdir) = env::var_os(STATE_TEST_SUBDIR_ENV)
        && !subdir.is_empty()
    {
        root.push(subdir);
    }
    Some(root)
}
