use crate::error::{Error, Result};
use serde_json::Value;
use std::{fs, path::Path};

pub(crate) struct FixtureInput {
    pub(crate) text: String,
    pub(crate) json: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FixtureKind {
    StateTest,
    BlockchainTest,
}

pub(crate) fn read(path: &Path) -> Result<FixtureInput> {
    let text = fs::read_to_string(path)
        .map_err(|source| Error::ReadInput { path: path.to_path_buf(), source })?;
    let json = serde_json::from_str(&text)
        .map_err(|source| Error::DecodeJson { path: path.to_path_buf(), source })?;
    Ok(FixtureInput { text, json })
}

pub(crate) fn detect(value: &Value) -> Option<FixtureKind> {
    let cases = value.as_object()?;
    let first = cases.values().find_map(Value::as_object)?;

    if has_any(first, &["blocks", "genesisBlockHeader", "lastblockhash", "network"]) {
        return Some(FixtureKind::BlockchainTest);
    }
    if has_any(first, &["env", "post", "transaction"]) {
        return Some(FixtureKind::StateTest);
    }
    None
}

pub(crate) fn entrypoints(value: &Value) -> Option<Vec<&str>> {
    let mut entrypoints = value.as_object()?.keys().map(String::as_str).collect::<Vec<_>>();
    entrypoints.sort_unstable();
    Some(entrypoints)
}

fn has_any(object: &serde_json::Map<String, Value>, fields: &[&str]) -> bool {
    fields.iter().any(|field| object.contains_key(*field))
}

#[cfg(test)]
mod tests {
    use super::{FixtureKind, detect, entrypoints};
    use serde_json::json;

    #[test]
    fn detects_blockchain_fixture() {
        let value = json!({"case": {"blocks": [], "pre": {}, "network": "Prague"}});
        assert_eq!(detect(&value), Some(FixtureKind::BlockchainTest));
    }

    #[test]
    fn detects_state_fixture() {
        let value = json!({"case": {"env": {}, "pre": {}, "post": {}, "transaction": {}}});
        assert_eq!(detect(&value), Some(FixtureKind::StateTest));
    }

    #[test]
    fn lists_top_level_case_names() {
        let value = json!({"b": {"env": {}}, "a": {"env": {}}});
        assert_eq!(entrypoints(&value).unwrap(), ["a", "b"]);
    }
}
