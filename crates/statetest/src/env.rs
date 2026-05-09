use std::{
    env,
    path::{Path, PathBuf},
};

/// Environment variable for the state test root.
pub(crate) const STATE_TEST_ROOT_ENV: &str = "EVM2_STATETEST_ROOT";

/// Fallback environment variable for the state test root.
pub(crate) const ETHEREUM_TESTS_ENV: &str = "ETHEREUM_TESTS";

/// revmc-compatible environment variable for ethereum/tests.
pub(crate) const ETHTESTS_ENV: &str = "ETHTESTS";

/// Environment variable for the downloaded fixture root.
pub(crate) const TEST_FIXTURES_ENV: &str = "EVM2_TEST_FIXTURES";

/// Environment variable for selecting stable EEST fixtures instead of develop.
pub(crate) const EEST_STABLE_ENV: &str = "EVM2_STATETEST_STABLE";

/// revmc-compatible environment variable for the downloaded fixture root.
pub(crate) const REVMC_TEST_FIXTURES_ENV: &str = "REVMC_TEST_FIXTURES";

/// Optional environment variable for selecting a subdirectory under the test root.
pub(crate) const STATE_TEST_SUBDIR_ENV: &str = "SUBDIR";

/// Repo-relative fixture root used by the setup script and CI.
pub(crate) const DEFAULT_FIXTURES_PATH: &str = "test-fixtures";

/// Repo-relative ethereum/tests checkout path supported for compatibility.
pub(crate) const DEFAULT_ETHEREUM_TESTS_PATH: &str = "tests/ethereum-tests";

/// A named state-test root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StateTestRoot {
    /// Stable root name used by the nextest harness.
    pub(crate) name: &'static str,
    /// Human readable root label.
    pub(crate) label: &'static str,
    /// Directory containing state-test JSON files.
    pub(crate) path: PathBuf,
}

/// Resolves the workspace root by walking up from this crate.
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("Cargo.toml").is_file() && path.join("crates").is_dir())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// Returns the root of downloaded state test fixtures.
pub(crate) fn fixtures_root() -> PathBuf {
    env::var_os(TEST_FIXTURES_ENV)
        .or_else(|| env::var_os(REVMC_TEST_FIXTURES_ENV))
        .map(PathBuf::from)
        .map(workspace_relative)
        .unwrap_or_else(|| workspace_root().join(DEFAULT_FIXTURES_PATH))
}

/// Returns the explicit state-test root configured through environment variables.
pub(crate) fn explicit_state_test_root_from_env() -> Option<PathBuf> {
    env::var_os(STATE_TEST_ROOT_ENV)
        .or_else(|| env::var_os(ETHEREUM_TESTS_ENV))
        .or_else(|| env::var_os(ETHTESTS_ENV))
        .map(PathBuf::from)
        .map(workspace_relative)
        .map(|mut x| {
            apply_subdir(&mut x);
            x
        })
}

/// Returns the state-test roots to run by default.
pub(crate) fn state_test_roots() -> Vec<StateTestRoot> {
    if let Some(path) = explicit_state_test_root_from_env() {
        return vec![StateTestRoot { name: "custom", label: "custom state tests", path }];
    }

    default_state_test_roots().into_iter().filter(|root| root.path.is_dir()).collect()
}

/// Returns the default repo-relative state-test roots, whether or not they exist.
pub(crate) fn default_state_test_roots() -> Vec<StateTestRoot> {
    let fixtures = fixtures_root();
    let ethereum_tests = workspace_root().join(DEFAULT_ETHEREUM_TESTS_PATH);
    let eest_path = if env_flag(EEST_STABLE_ENV) {
        fixtures.join("main/stable/state_tests")
    } else {
        fixtures.join("main/develop/state_tests")
    };
    let mut roots = vec![
        StateTestRoot { name: "eest", label: "execution-spec-tests", path: eest_path },
        StateTestRoot {
            name: "devnet",
            label: "execution-spec-tests devnet",
            path: fixtures.join("devnet/state_tests"),
        },
        StateTestRoot {
            name: "legacy_cancun",
            label: "legacy Cancun",
            path: fixtures.join("legacytests/Cancun/GeneralStateTests"),
        },
        StateTestRoot {
            name: "legacy_constantinople",
            label: "legacy Constantinople",
            path: fixtures.join("legacytests/Constantinople/GeneralStateTests"),
        },
    ];

    if let Some(path) = general_state_tests_path(&ethereum_tests) {
        roots.push(StateTestRoot {
            name: "ethereum_tests",
            label: "ethereum/tests GeneralStateTests",
            path,
        });
    }

    for root in &mut roots {
        apply_subdir(&mut root.path);
    }
    roots
}

fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty() && value.to_str() != Some("0"))
}

fn general_state_tests_path(root: &Path) -> Option<PathBuf> {
    let sibling = root.parent().map(|parent| parent.join("GeneralStateTests"));
    if let Some(path) = sibling
        && path.is_dir()
    {
        return Some(path);
    }

    let path = root.join("GeneralStateTests");
    path.is_dir().then_some(path)
}

fn apply_subdir(root: &mut PathBuf) {
    if let Some(subdir) = env::var_os(STATE_TEST_SUBDIR_ENV)
        && !subdir.is_empty()
    {
        root.push(subdir);
    }
}

fn workspace_relative(path: PathBuf) -> PathBuf {
    if path.is_absolute() { path } else { workspace_root().join(path) }
}
