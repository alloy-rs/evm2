//! Additional-tests suite.
//!
//! When `EVM2_ADDITIONAL_TESTS` points at a folder (or file), every JSON file
//! found anywhere under it runs as one suite whose kind (state vs blockchain) is
//! detected per file, so no test-name filter is needed to isolate it.

use crate::{
    blockchaintest::{ExecuteConfig as BlockchainConfig, NoopHook, execute_str},
    execute::{self, ExecuteConfig as StateConfig},
    filter::NameFilter,
    fixture_io,
    fixture_kind::FixtureKind,
    harness::{TestRoot, TestSuite, descend_all},
};
use libtest_mimic::Failed;
use std::path::PathBuf;

/// Builds the auto-detecting suite rooted at `path`.
pub(crate) fn suite(path: PathBuf) -> TestSuite {
    TestSuite {
        name: "additional",
        roots: vec![TestRoot { name: "additional".to_string(), label: "additional tests", path }],
        // Descend into every directory so all JSON files under the path run.
        should_descend: descend_all,
        should_ignore,
        run_file,
        run_files: None,
    }
}

/// Honors the same ignore lists the state and blockchain runners use, since an
/// additional path can hold fixtures of either kind.
fn should_ignore(name: &str) -> bool {
    crate::runner::IGNORED_TESTS
        .iter()
        .chain(crate::blockchaintest::IGNORED_TESTS)
        .any(|pattern| name.contains(pattern))
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    let input =
        fixture_io::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    match FixtureKind::from_json(&input).map_err(|err| format!("{}: {err}", path.display()))? {
        Some(FixtureKind::Blockchain) => execute_str(
            &path,
            &input,
            BlockchainConfig {
                compare_state_root: true,
                compare_receipt_root: true,
                ..Default::default()
            },
            &NameFilter::default(),
            &mut NoopHook,
        )
        .map(|_| ())
        .map_err(|err| err.to_string().into()),
        Some(FixtureKind::State) => {
            execute::execute_str_with_config(&path, &input, StateConfig::default())
                .map(|_| ())
                .map_err(|err| err.to_string().into())
        }
        None => Err(format!("could not detect fixture kind in {}", path.display()).into()),
    }
}
