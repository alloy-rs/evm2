use crate::harness::TestRoot;
use std::{
    env,
    path::{Path, PathBuf},
};

/// Environment variable for the downloaded fixture root.
pub(crate) const TEST_FIXTURES_ENV: &str = "EVM2_TEST_FIXTURES";

/// Generic EEST stable selector.
pub(crate) const EEST_STABLE_ENV: &str = "EVM2_EEST_STABLE";

/// Selector for the EEST `main` suite, which the setup script downloads only when it is set.
pub(crate) const EEST_MAIN_ENV: &str = "EVM2_STATETEST_MAIN";

/// Selector for devnet-only runs, where the setup script skips the legacy fixtures.
pub(crate) const DEVNET_ONLY_ENV: &str = "EVM2_STATETEST_DEVNET_ONLY";

/// Repo-relative fixture root used by the setup script and CI.
pub(crate) const DEFAULT_FIXTURES_PATH: &str = "test-fixtures";

/// Environment variable pointing at additional tests to run: a folder (or file)
/// whose fixtures execute as state or blockchain tests by per-file field
/// detection.
pub(crate) const ADDITIONAL_TESTS_ENV: &str = "EVM2_ADDITIONAL_TESTS";

/// Returns the additional-tests path configured through the environment.
pub(crate) fn additional_tests_path() -> Option<PathBuf> {
    env::var_os(ADDITIONAL_TESTS_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(workspace_relative)
}

/// Resolves the workspace root by walking up from this crate.
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("Cargo.toml").is_file() && path.join("crates").is_dir())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// Returns the root of downloaded test fixtures.
pub(crate) fn fixtures_root() -> PathBuf {
    env::var_os(TEST_FIXTURES_ENV)
        .map(PathBuf::from)
        .map(workspace_relative)
        .unwrap_or_else(|| workspace_root().join(DEFAULT_FIXTURES_PATH))
}

/// Returns whether an environment flag is set to a truthy value.
pub(crate) fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty() && value.to_str() != Some("0"))
}

/// Appends `SUBDIR`-style filters to a root path.
pub(crate) fn apply_subdir(root: &mut PathBuf, name: &str) {
    if let Some(subdir) = env::var_os(name)
        && !subdir.is_empty()
    {
        root.push(subdir);
    }
}

/// Appends a `SUBDIR`-style filter to every root, dropping roots without that subdirectory.
pub(crate) fn apply_subdir_to_roots(roots: &mut Vec<TestRoot>, name: &str) {
    for root in roots.iter_mut() {
        apply_subdir(&mut root.path, name);
    }
    if env::var_os(name).is_some_and(|subdir| !subdir.is_empty()) {
        roots.retain(|root| root.path.is_dir());
    }
}

/// Resolves a path against the workspace root if it is relative.
pub(crate) fn workspace_relative(path: PathBuf) -> PathBuf {
    if path.is_absolute() { path } else { workspace_root().join(path) }
}
