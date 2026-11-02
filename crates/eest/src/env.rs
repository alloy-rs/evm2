use crate::{
    blockchaintest::env::explicit_blockchain_test_root_from_env,
    fixtures::{
        EEST_ONLY_ENV, EEST_STABLE_ENV, apply_subdir, apply_subdir_to_roots, env_flag,
        fixtures_root, workspace_relative,
    },
    harness::TestRoot,
};
use std::{env, path::PathBuf};

/// Environment variable for the state test root.
pub(crate) const STATE_TEST_ROOT_ENV: &str = "EVM2_STATETEST_ROOT";

/// Optional environment variable for selecting a subdirectory under the test root.
pub(crate) const STATE_TEST_SUBDIR_ENV: &str = "SUBDIR";

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
    let eest_path = if env_flag(EEST_STABLE_ENV) {
        fixtures.join("main/stable/state_tests")
    } else {
        fixtures.join("main/develop/state_tests")
    };
    let mut roots = vec![StateTestRoot {
        name: "statetests::eest".to_string(),
        label: "execution-specs release",
        path: fixtures.join("eest/state_tests"),
    }];

    if eest_path.is_dir() {
        roots.push(StateTestRoot {
            name: "statetests".to_string(),
            label: "execution-spec-tests",
            path: eest_path,
        });
    }

    if !env_flag(EEST_ONLY_ENV) {
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

    apply_subdir_to_roots(&mut roots, STATE_TEST_SUBDIR_ENV);
    roots
}
