use crate::error::{Error, Result};
use evm2_eest::FixtureKind;
use serde::de::IgnoredAny;
use std::{collections::BTreeMap, path::Path};

pub(crate) fn read_blockchain(path: &Path) -> Result<evm2_eest::blockchaintest::BlockchainTest> {
    evm2_eest::read_blockchain_fixture(path)
        .map_err(|source| Error::DecodeFixture { path: path.to_path_buf(), source })
}

pub(crate) fn read_text(path: &Path) -> Result<String> {
    evm2_eest::read_fixture_text(path)
        .map_err(|source| Error::ReadInput { path: path.to_path_buf(), source })
}

/// Detects the kind of a JSON fixture from its text.
pub(crate) fn detect_str(path: &Path, input: &str) -> Result<Option<FixtureKind>> {
    FixtureKind::from_json(input)
        .map_err(|source| Error::DecodeJson { path: path.to_path_buf(), source })
}

/// Returns the sorted top-level test names of a JSON fixture.
pub(crate) fn test_names(path: &Path, input: &str) -> Result<Vec<String>> {
    let cases = serde_json::from_str::<BTreeMap<String, IgnoredAny>>(input)
        .map_err(|source| Error::DecodeJson { path: path.to_path_buf(), source })?;
    Ok(cases.into_keys().collect())
}
