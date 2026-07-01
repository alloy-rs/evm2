use crate::fuzzer::case::EvmCase;
use alloy_primitives::keccak256;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn read_case(path: &Path) -> Result<EvmCase, String> {
    let contents = fs::read_to_string(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))
}

pub(crate) fn write_failure_case(
    seed: u64,
    case_index: u64,
    case: &EvmCase,
) -> Result<PathBuf, String> {
    let (path, existed) = write_hashed_case("case", case)?;
    if existed {
        eprintln!("duplicate failing case for seed {seed}, case {case_index}: {}", path.display());
    }
    Ok(path)
}

pub fn write_minimized_case(case: &EvmCase) -> Result<PathBuf, String> {
    write_hashed_case("minimized-case", case).map(|(path, _)| path)
}

fn write_hashed_case(prefix: &str, case: &EvmCase) -> Result<(PathBuf, bool), String> {
    let dir = PathBuf::from("crates/cli/fuzzer/corpus/failures");
    fs::create_dir_all(&dir).map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    let canonical = serde_json::to_vec(case)
        .map_err(|err| format!("failed to serialize case for hashing: {err}"))?;
    let hash = keccak256(&canonical);
    let path = dir.join(format!("{prefix}-{hash}.json"));
    if path.exists() {
        return Ok((path, true));
    }
    let json = serde_json::to_string_pretty(case)
        .map_err(|err| format!("failed to serialize {}: {err}", path.display()))?;
    fs::write(&path, json).map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    Ok((path, false))
}

pub fn case_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    collect_case_paths(path, &mut paths)
        .map_err(|err| format!("failed to scan {}: {err}", path.display()))?;
    Ok(paths)
}

fn collect_case_paths(path: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_file() {
        if path.extension().is_some_and(|extension| extension == "json") {
            paths.push(path.to_owned());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_case_paths(&path, paths)?;
        } else if path.extension().is_some_and(|extension| extension == "json") {
            paths.push(path);
        }
    }
    Ok(())
}
