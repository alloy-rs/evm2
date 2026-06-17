use std::{
    env,
    path::{Path, PathBuf},
};

/// Environment variable for the downloaded fixture root.
pub(crate) const TEST_FIXTURES_ENV: &str = "EVM2_TEST_FIXTURES";

/// Generic EEST stable selector.
pub(crate) const EEST_STABLE_ENV: &str = "EVM2_EEST_STABLE";

/// Repo-relative fixture root used by the setup script and CI.
pub(crate) const DEFAULT_FIXTURES_PATH: &str = "test-fixtures";

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

/// Resolves a path against the workspace root if it is relative.
pub(crate) fn workspace_relative(path: PathBuf) -> PathBuf {
    if path.is_absolute() { path } else { workspace_root().join(path) }
}
