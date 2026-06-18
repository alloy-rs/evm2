use crate::blockchaintest::BlockchainTest;
use std::{
    fs::{self, File},
    io::{self, BufWriter, Cursor},
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
    /// CBOR decoding failed.
    #[error("CBOR decoding failed: {0}")]
    Cbor(#[from] ciborium::de::Error<io::Error>),
    /// CBOR input had bytes remaining after decoding.
    #[error("CBOR decoding failed: trailing bytes remain after deserialization")]
    CborTrailingBytes,
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
    /// CBOR encoding failed.
    #[error("CBOR encoding failed: {0}")]
    Cbor(#[from] ciborium::ser::Error<io::Error>),
}

/// Returns true when the path uses the binary fixture extension.
pub fn is_binary_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "bin")
}

/// Reads a blockchain test fixture from JSON or CBOR binary.
pub fn read_blockchain(path: &Path) -> Result<BlockchainTest, FixtureReadError> {
    if is_binary_path(path) {
        let bytes = fs::read(path)?;
        let mut reader = Cursor::new(&bytes);
        let suite = ciborium::from_reader(&mut reader)?;
        if reader.position() == bytes.len() as u64 {
            Ok(suite)
        } else {
            Err(FixtureReadError::CborTrailingBytes)
        }
    } else {
        let input = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&input)?)
    }
}

/// Writes a blockchain test fixture as JSON or CBOR binary.
pub fn write_blockchain(path: &Path, suite: &BlockchainTest) -> Result<(), FixtureWriteError> {
    if is_binary_path(path) {
        let writer = BufWriter::new(File::create(path)?);
        ciborium::into_writer(suite, writer)?;
    } else {
        let writer = BufWriter::new(File::create(path)?);
        serde_json::to_writer(writer, suite)?;
    }
    Ok(())
}
