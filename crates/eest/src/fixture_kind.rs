//! Detection of the kind of an EEST fixture file.

use crate::fixture_io;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use std::{fmt, path::Path};

/// Case fields only blockchain tests have.
const BLOCKCHAIN_FIELDS: &[&str] = &["blocks", "genesisBlockHeader", "lastblockhash", "network"];

/// Case fields only state tests have.
const STATE_FIELDS: &[&str] = &["env", "post", "transaction"];

/// The kind of an EEST fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureKind {
    /// A state test.
    State,
    /// A blockchain test, in JSON or postcard binary form.
    Blockchain,
}

impl FixtureKind {
    /// Returns the kind of a binary fixture from its path, or `None` for any other path.
    ///
    /// The binary format has no header to sniff, and only holds blockchain tests.
    pub fn from_path(path: &Path) -> Option<Self> {
        fixture_io::is_binary_path(path).then_some(Self::Blockchain)
    }

    /// Detects the kind of a JSON fixture from the fields of its first case.
    ///
    /// A case with any of `blocks`, `genesisBlockHeader`, `lastblockhash` or `network` is a
    /// blockchain test; otherwise a case with any of `env`, `post` or `transaction` is a state
    /// test. Only the first case is inspected, and the rest of the input is skipped without
    /// being materialized. Returns `Ok(None)` when the first case has none of these fields, or
    /// the input has no case.
    pub fn from_json(input: &str) -> Result<Option<Self>, serde_json::Error> {
        serde_json::Deserializer::from_str(input).deserialize_map(FixtureVisitor)
    }
}

struct FixtureVisitor;

impl<'de> Visitor<'de> for FixtureVisitor {
    type Value = Option<FixtureKind>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an EEST fixture object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut kind = None;
        let mut seen_case = false;
        while map.next_key::<IgnoredAny>()?.is_some() {
            if seen_case {
                map.next_value::<IgnoredAny>()?;
            } else {
                seen_case = true;
                kind = map.next_value_seed(CaseSeed)?;
            }
        }
        Ok(kind)
    }
}

struct CaseSeed;

impl<'de> DeserializeSeed<'de> for CaseSeed {
    type Value = Option<FixtureKind>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for CaseSeed {
    type Value = Option<FixtureKind>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an EEST fixture case object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut blockchain = false;
        let mut state = false;
        while let Some(field) = map.next_key::<&str>()? {
            blockchain |= BLOCKCHAIN_FIELDS.contains(&field);
            state |= STATE_FIELDS.contains(&field);
            map.next_value::<IgnoredAny>()?;
        }
        Ok(if blockchain {
            Some(FixtureKind::Blockchain)
        } else if state {
            Some(FixtureKind::State)
        } else {
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_state_json() {
        let input = r#"{"case": {"env": {}, "pre": {}, "post": {}, "transaction": {}}}"#;
        assert_eq!(FixtureKind::from_json(input).unwrap(), Some(FixtureKind::State));
    }

    #[test]
    fn detects_blockchain_json() {
        let input = r#"{"case": {"blocks": [], "pre": {}, "network": "Prague"}}"#;
        assert_eq!(FixtureKind::from_json(input).unwrap(), Some(FixtureKind::Blockchain));
    }

    #[test]
    fn detects_binary_path() {
        for path in ["fixture.bin", "fixture.bin.zst"] {
            assert_eq!(FixtureKind::from_path(Path::new(path)), Some(FixtureKind::Blockchain));
        }
        for path in ["fixture.json", "fixture.json.zst"] {
            assert_eq!(FixtureKind::from_path(Path::new(path)), None);
        }
    }

    #[test]
    fn rejects_unknown_input() {
        assert_eq!(FixtureKind::from_json("{}").unwrap(), None);
        assert_eq!(FixtureKind::from_json(r#"{"case": {"pre": {}}}"#).unwrap(), None);
        assert!(FixtureKind::from_json("[]").is_err());
        assert!(FixtureKind::from_json("not json").is_err());
    }

    #[test]
    fn inspects_only_the_first_case() {
        let input = r#"{"a": {"pre": {}}, "b": {"env": {}}}"#;
        assert_eq!(FixtureKind::from_json(input).unwrap(), None);
    }
}
