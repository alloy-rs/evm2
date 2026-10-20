use crate::{
    blockchaintest::env::explicit_blockchain_test_root_from_env,
    fixtures::{
        DEVNET_ONLY_ENV, EEST_STABLE_ENV, apply_subdir, apply_subdir_to_roots, env_flag,
        fixtures_root, workspace_relative, workspace_root,
    },
    harness::TestRoot,
};
use std::{
    env,
    path::{Path, PathBuf},
};

/// Environment variable for the state test root.
pub(crate) const STATE_TEST_ROOT_ENV: &str = "EVM2_STATETEST_ROOT";

/// Environment variable for selecting stable EEST fixtures instead of develop.
pub(crate) const STATETEST_STABLE_ENV: &str = "EVM2_STATETEST_STABLE";

/// Optional environment variable for selecting a subdirectory under the test root.
pub(crate) const STATE_TEST_SUBDIR_ENV: &str = "SUBDIR";

/// Optional repo-relative ethereum/tests checkout path.
pub(crate) const DEFAULT_ETHEREUM_TESTS_PATH: &str = "tests/ethereum-tests";

/// A named state-test root.
pub(crate) type StateTestRoot = TestRoot;

/// Returns the explicit state-test root configured through environment variables.
pub(crate) fn explicit_state_test_root_from_env() -> Option<PathBuf> {
    env::var_os(STATE_TEST_ROOT_ENV).map(PathBuf::from).map(workspace_relative).map(|mut x| {
        apply_subdir(&mut x, STATE_TEST_SUBDIR_ENV);
        x
    })
}

/// Returns the state-test roots to run by default.
///
/// A missing root that the setup script downloads by default is an error, not a skip, unless the
/// blockchain tests got an explicit root.
pub(crate) fn state_test_roots() -> Vec<StateTestRoot> {
    if let Some(path) = explicit_state_test_root_from_env() {
        return vec![StateTestRoot {
            name: "statetests::custom".to_string(),
            label: "custom state tests",
            path,
        }];
    }

    let mut roots = default_state_test_roots();
    if explicit_blockchain_test_root_from_env().is_some() {
        roots.retain(|root| root.path.is_dir());
    }
    roots
}

/// Returns the default repo-relative state-test roots. Optional roots are included only if they
/// exist.
pub(crate) fn default_state_test_roots() -> Vec<StateTestRoot> {
    let fixtures = fixtures_root();
    let ethereum_tests = workspace_root().join(DEFAULT_ETHEREUM_TESTS_PATH);
    let eest_path = if env_flag(STATETEST_STABLE_ENV) || env_flag(EEST_STABLE_ENV) {
        fixtures.join("main/stable/state_tests")
    } else {
        fixtures.join("main/develop/state_tests")
    };
    let mut roots = vec![StateTestRoot {
        name: "statetests::devnet".to_string(),
        label: "execution-spec-tests devnet",
        path: fixtures.join("devnet/state_tests"),
    }];

    if eest_path.is_dir() {
        roots.push(StateTestRoot {
            name: "statetests".to_string(),
            label: "execution-spec-tests",
            path: eest_path,
        });
    }

    if !env_flag(DEVNET_ONLY_ENV) {
        roots.push(StateTestRoot {
            name: "legacy::cancun".to_string(),
            label: "legacy Cancun",
            path: fixtures.join("legacytests/Cancun/GeneralStateTests"),
        });
        roots.push(StateTestRoot {
            name: "legacy::constantinople".to_string(),
            label: "legacy Constantinople",
            path: fixtures.join("legacytests/Constantinople/GeneralStateTests"),
        });
    }

    if let Some(path) = general_state_tests_path(&ethereum_tests) {
        roots.push(StateTestRoot {
            name: "legacy::ethereum_tests".to_string(),
            label: "ethereum/tests GeneralStateTests",
            path,
        });
    }

    apply_subdir_to_roots(&mut roots, STATE_TEST_SUBDIR_ENV);
    roots
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
