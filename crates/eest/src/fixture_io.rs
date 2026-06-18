use crate::{binary, blockchaintest::BlockchainTest};
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
    /// Postcard decoding failed.
    #[error("postcard decoding failed: {0}")]
    Postcard(#[from] postcard::Error),
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
    /// Postcard encoding failed.
    #[error("postcard encoding failed: {0}")]
    Postcard(#[from] postcard::Error),
}

/// Returns true when the path uses the binary fixture extension.
pub fn is_binary_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "bin")
}

/// Reads a blockchain test fixture from JSON or postcard binary.
pub fn read_blockchain(path: &Path) -> Result<BlockchainTest, FixtureReadError> {
    if is_binary_path(path) {
        let bytes = fs::read(path)?;
        Ok(binary::from_bytes(&bytes)?)
    } else {
        let input = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&input)?)
    }
}

/// Writes a blockchain test fixture as JSON or postcard binary.
pub fn write_blockchain(path: &Path, suite: &BlockchainTest) -> Result<(), FixtureWriteError> {
    if is_binary_path(path) {
        fs::write(path, binary::to_vec(suite)?)?;
    } else {
        let writer = BufWriter::new(File::create(path)?);
        serde_json::to_writer(writer, suite)?;
    }
    Ok(())
}
