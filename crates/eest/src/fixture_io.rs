use crate::blockchaintest::BlockchainTest;
use serde::de::DeserializeOwned;
use std::{
    fs::{self, File},
    io::{self, BufWriter},
    path::Path,
};
use thiserror::Error;

/// Error while reading an EEST fixture.
#[derive(Debug, Error)]
pub enum FixtureReadError {
    /// File read failed.
    #[error("read error: {0}")]
    Read(#[from] io::Error),
    /// JSON decoding failed.
    #[error("JSON decoding failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Wincode decoding failed.
    #[error("wincode decoding failed: {0}")]
    Wincode(#[from] wincode::error::ReadError),
}

/// Error while writing an EEST fixture.
#[derive(Debug, Error)]
pub enum FixtureWriteError {
    /// File write failed.
    #[error("write error: {0}")]
    Write(#[from] io::Error),
    /// JSON encoding failed.
    #[error("JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Wincode encoding failed.
    #[error("wincode encoding failed: {0}")]
    Wincode(#[from] wincode::error::WriteError),
}

/// Returns true when the path uses the wincode binary fixture extension.
pub fn is_wincode_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "bin")
}

/// Reads a JSON fixture or a wincode binary fixture.
pub(crate) fn read_json<T>(path: &Path) -> Result<T, FixtureReadError>
where
    T: DeserializeOwned,
{
    let input = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&input)?)
}

/// Reads a blockchain test fixture from JSON or wincode binary.
pub fn read_blockchain(path: &Path) -> Result<BlockchainTest, FixtureReadError> {
    if is_wincode_path(path) {
        let bytes = fs::read(path)?;
        Ok(wincode::deserialize_exact(&bytes)?)
    } else {
        read_json(path)
    }
}

/// Writes a blockchain test fixture as JSON or wincode binary.
pub fn write_blockchain(path: &Path, suite: &BlockchainTest) -> Result<(), FixtureWriteError> {
    if is_wincode_path(path) {
        let bytes = wincode::serialize(suite)?;
        fs::write(path, bytes)?;
    } else {
        let writer = BufWriter::new(File::create(path)?);
        serde_json::to_writer(writer, suite)?;
    }
    Ok(())
}
