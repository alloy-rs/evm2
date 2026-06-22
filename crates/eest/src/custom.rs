//! Single-path fixture suite.
//!
//! When `EVM2_FIXTURE_PATH` points at a folder (or file), every JSON fixture
//! found under it runs as one suite whose kind (state vs blockchain) is detected
//! per file, so no test-name filter is needed to isolate it.

use crate::{
    blockchaintest::{self, ExecuteConfig as BlockchainConfig, NoopHook},
    execute::{self, ExecuteConfig as StateConfig},
    filter::EntryPoint,
    fixture_io,
    harness::{TestRoot, TestSuite},
};
use libtest_mimic::Failed;
use std::path::{Path, PathBuf};

/// Builds the auto-detecting suite rooted at `path`.
pub(crate) fn suite(path: PathBuf) -> TestSuite {
    TestSuite {
        name: "fixtures",
        roots: vec![TestRoot { name: "fixtures", label: "custom fixtures", path }],
        should_descend,
        should_ignore,
        run_file,
    }
}

/// Skips fixture directories this runner cannot execute: transaction tests and
/// the engine/sync blockchain variants.
fn should_descend(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    !matches!(
        name,
        "transaction_tests"
            | "blockchain_tests_engine"
            | "blockchain_tests_engine_x"
            | "blockchain_tests_sync"
    )
}

/// Skips fixtures that either suite would skip, keeping behavior consistent with
/// the default runs.
fn should_ignore(name: &str) -> bool {
    crate::runner::should_ignore(name) || blockchaintest::runner::should_ignore(name)
}

fn run_file(path: PathBuf) -> Result<(), Failed> {
    let input =
        fixture_io::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    match detect_kind(&input) {
        Some(FixtureKind::Blockchain) => blockchaintest::execute_str(
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
