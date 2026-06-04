use crate::error::{Error, Result};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;
use std::{fmt, fs, path::Path};

pub(crate) struct FixtureInput {
    pub(crate) json: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FixtureKind {
    StateTest,
    BlockchainTest,
}

pub(crate) fn read(path: &Path) -> Result<FixtureInput> {
    let text = read_text(path)?;
    let json = serde_json::from_str(&text)
        .map_err(|source| Error::DecodeJson { path: path.to_path_buf(), source })?;
    Ok(FixtureInput { json })
}

pub(crate) fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|source| Error::ReadInput { path: path.to_path_buf(), source })
}

pub(crate) fn detect_str(path: &Path, input: &str) -> Result<Option<FixtureKind>> {
    struct FixtureKindVisitor;

    impl<'de> Visitor<'de> for FixtureKindVisitor {
        type Value = Option<FixtureKind>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an EEST fixture object")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut kind = None;
            while map.next_key::<IgnoredAny>()?.is_some() {
                if kind.is_none() {
                    kind = map.next_value_seed(FixtureCaseVisitor)?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(kind)
        }
    }

    struct FixtureCaseVisitor;

    impl<'de> DeserializeSeed<'de> for FixtureCaseVisitor {
        type Value = Option<FixtureKind>;

        fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for FixtureCaseVisitor {
        type Value = Option<FixtureKind>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an EEST fixture case object")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut is_blockchain = false;
            let mut is_state = false;
            while let Some(field) = map.next_key::<&str>()? {
                is_blockchain |=
                    matches!(field, "blocks" | "genesisBlockHeader" | "lastblockhash" | "network");
                is_state |= matches!(field, "env" | "post" | "transaction");
                map.next_value::<IgnoredAny>()?;
            }
            Ok(if is_blockchain {
                Some(FixtureKind::BlockchainTest)
            } else if is_state {
                Some(FixtureKind::StateTest)
            } else {
                None
            })
        }
    }

    serde_json::Deserializer::from_str(input)
        .deserialize_any(FixtureKindVisitor)
        .map_err(|source| Error::DecodeJson { path: path.to_path_buf(), source })
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
    use super::{FixtureKind, detect, detect_str, entrypoints};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn detects_blockchain_fixture() {
        let value = json!({"case": {"blocks": [], "pre": {}, "network": "Prague"}});
        assert_eq!(detect(&value), Some(FixtureKind::BlockchainTest));
        assert_eq!(
            detect_str(Path::new("fixture.json"), &value.to_string()).unwrap(),
            Some(FixtureKind::BlockchainTest)
        );
    }

    #[test]
    fn detects_state_fixture() {
        let value = json!({"case": {"env": {}, "pre": {}, "post": {}, "transaction": {}}});
        assert_eq!(detect(&value), Some(FixtureKind::StateTest));
        assert_eq!(
            detect_str(Path::new("fixture.json"), &value.to_string()).unwrap(),
            Some(FixtureKind::StateTest)
        );
    }

    #[test]
    fn lists_top_level_case_names() {
        let value = json!({"b": {"env": {}}, "a": {"env": {}}});
        assert_eq!(entrypoints(&value).unwrap(), ["a", "b"]);
    }
}
