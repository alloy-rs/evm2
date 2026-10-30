use crate::{
    env::explicit_state_test_root_from_env,
    fixtures::{
        EEST_STABLE_ENV, apply_subdir, apply_subdir_to_roots, env_flag, fixtures_root,
        workspace_relative,
    },
    harness::TestRoot,
};
use std::{env, path::PathBuf};

/// Environment variable for the blockchain test root.
pub(crate) const BLOCKCHAIN_TEST_ROOT_ENV: &str = "EVM2_BLOCKCHAINTEST_ROOT";

/// Optional environment variable for selecting a subdirectory under the test root.
pub(crate) const BLOCKCHAIN_TEST_SUBDIR_ENV: &str = "SUBDIR";

/// A named blockchain-test root.
pub(crate) type BlockchainTestRoot = TestRoot;

/// Returns the explicit blockchain-test root configured through environment variables.
pub(crate) fn explicit_blockchain_test_root_from_env() -> Option<PathBuf> {
    env::var_os(BLOCKCHAIN_TEST_ROOT_ENV).map(PathBuf::from).map(workspace_relative).map(
        |mut path| {
            apply_subdir(&mut path, BLOCKCHAIN_TEST_SUBDIR_ENV);
            path
        },
    )
}

/// Returns the blockchain-test roots to run by default.
///
/// A missing root that the setup script downloads by default is an error, not a skip, unless the
/// state tests got an explicit root.
pub(crate) fn blockchain_test_roots() -> Vec<BlockchainTestRoot> {
    if let Some(path) = explicit_blockchain_test_root_from_env() {
        return vec![BlockchainTestRoot {
            name: "blockchain_tests::custom".to_string(),
            label: "custom blockchain tests",
            path,
        }];
    }

    let mut roots = default_blockchain_test_roots();
    if explicit_state_test_root_from_env().is_some() {
        roots.retain(|root| root.path.is_dir());
    }
    roots
}

/// Returns the default repo-relative blockchain-test roots. The `main` root is optional.
pub(crate) fn default_blockchain_test_roots() -> Vec<BlockchainTestRoot> {
    let fixtures = fixtures_root();
    let main_path = if env_flag(EEST_STABLE_ENV) {
        fixtures.join("main/stable/blockchain_tests")
    } else {
        fixtures.join("main/develop/blockchain_tests")
    };

    let mut roots = vec![BlockchainTestRoot {
        name: "blockchain_tests::eest".to_string(),
        label: "execution-specs release",
        path: fixtures.join("eest/blockchain_tests"),
    }];

    if main_path.is_dir() {
        roots.push(BlockchainTestRoot {
            name: "blockchain_tests".to_string(),
            label: "execution-spec-tests",
            path: main_path,
        });
    }

    apply_subdir_to_roots(&mut roots, BLOCKCHAIN_TEST_SUBDIR_ENV);
    roots
}
