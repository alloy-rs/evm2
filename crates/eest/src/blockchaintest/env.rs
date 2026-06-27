use crate::{
    fixtures::{EEST_STABLE_ENV, apply_subdir, env_flag, fixtures_root, workspace_relative},
    harness::TestRoot,
};
use std::{env, path::PathBuf};

/// Environment variable for the blockchain test root.
pub(crate) const BLOCKCHAIN_TEST_ROOT_ENV: &str = "EVM2_BLOCKCHAINTEST_ROOT";

/// Environment variable for selecting stable EEST fixtures instead of develop.
pub(crate) const BLOCKCHAINTEST_STABLE_ENV: &str = "EVM2_BLOCKCHAINTEST_STABLE";

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
pub(crate) fn blockchain_test_roots() -> Vec<BlockchainTestRoot> {
    if let Some(path) = explicit_blockchain_test_root_from_env() {
        return vec![BlockchainTestRoot {
            name: "blockchain_tests::custom".to_string(),
            label: "custom blockchain tests",
            path,
        }];
    }

    default_blockchain_test_roots().into_iter().filter(|root| root.path.is_dir()).collect()
}

/// Returns the default repo-relative blockchain-test roots, whether or not they exist.
pub(crate) fn default_blockchain_test_roots() -> Vec<BlockchainTestRoot> {
    let fixtures = fixtures_root();
    let main_path = if env_flag(BLOCKCHAINTEST_STABLE_ENV) || env_flag(EEST_STABLE_ENV) {
        fixtures.join("main/stable/blockchain_tests")
    } else {
        fixtures.join("main/develop/blockchain_tests")
    };

    let mut roots = vec![
        BlockchainTestRoot {
            name: "blockchain_tests".to_string(),
            label: "execution-spec-tests",
            path: main_path,
        },
        BlockchainTestRoot {
            name: "blockchain_tests::devnet".to_string(),
            label: "execution-spec-tests devnet",
            path: fixtures.join("devnet/blockchain_tests"),
        },
    ];

    for root in &mut roots {
        apply_subdir(&mut root.path, BLOCKCHAIN_TEST_SUBDIR_ENV);
    }
    roots
}
