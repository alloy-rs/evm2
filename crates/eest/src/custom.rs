//! Single-path fixture suite.
//!
//! When `EVM2_FIXTURE_PATH` points at a folder (or file), every JSON fixture
//! found anywhere under it runs as one suite whose kind (state vs blockchain) is
//! detected per file, so no test-name filter is needed to isolate it.

use crate::{
    blockchaintest::{ExecuteConfig as BlockchainConfig, NoopHook, execute_str},
    execute::{self, ExecuteConfig as StateConfig},
    filter::EntryPoint,
    fixture_io,
    harness::{TestRoot, TestSuite, descend_all},
};
use libtest_mimic::Failed;
use std::path::PathBuf;

/// Builds the auto-detecting suite rooted at `path`.
pub(crate) fn suite(path: PathBuf) -> TestSuite {
    TestSuite {
        name: "fixtures",
        roots: vec![TestRoot { name: "fixtures", label: "custom fixtures", path }],
        // Descend into every directory so all JSON files under the path run.
        should_descend: descend_all,
        should_ignore: ignore_none,
        run_file,
    }
}

/// Runs every JSON file: a custom path is an explicit request, so nothing is
/// skipped.
const fn ignore_none(_name: &str) -> bool {
    false
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    let input =
        fixture_io::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    match detect_kind(&input) {
        Some(FixtureKind::Blockchain) => execute_str(
            &path,
            &input,
            BlockchainConfig::default(),
            &EntryPoint::default(),
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

enum FixtureKind {
    State,
    Blockchain,
}

/// Detects a fixture's kind from the fields of its first case.
fn detect_kind(input: &str) -> Option<FixtureKind> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    let first = value.as_object()?.values().find_map(serde_json::Value::as_object)?;
    if has_any(first, &["blocks", "genesisBlockHeader", "lastblockhash", "network"]) {
        Some(FixtureKind::Blockchain)
    } else if has_any(first, &["env", "post", "transaction"]) {
        Some(FixtureKind::State)
    } else {
        None
    }
}

fn has_any(object: &serde_json::Map<String, serde_json::Value>, fields: &[&str]) -> bool {
    fields.iter().any(|field| object.contains_key(*field))
}
