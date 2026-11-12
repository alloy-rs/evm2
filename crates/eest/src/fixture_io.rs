use crate::{binary, blockchaintest::BlockchainTest};
use std::{
    fs::{self, File},
    io::{self, BufWriter, Read, Write},
    path::Path,
};
use thiserror::Error;
use zstd::stream::{read::Decoder as ZstdDecoder, write::Encoder as ZstdEncoder};

const ZSTD_COMPRESSION_LEVEL: i32 = 19;

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
pub(crate) fn is_binary_path(path: &Path) -> bool {
    has_extension(path, "bin") || is_zstd_path(path) && stem_has_extension(path, "bin")
}

/// Reads a plain JSON fixture or zstd-compressed JSON fixture.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader: Box<dyn Read> =
        if is_zstd_path(path) { Box::new(ZstdDecoder::new(file)?) } else { Box::new(file) };
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    Ok(input)
}

/// Reads a blockchain test fixture from JSON, zstd-compressed JSON, or postcard binary.
pub fn read_blockchain(path: &Path) -> Result<BlockchainTest, FixtureReadError> {
    if is_binary_path(path) {
        let bytes = read_bytes(path)?;
        Ok(binary::from_bytes(&bytes)?)
    } else {
        let input = read_to_string(path)?;
        Ok(serde_json::from_str(&input)?)
    }
}

/// Writes a blockchain test fixture as JSON, zstd-compressed JSON, or postcard binary.
pub fn write_blockchain(path: &Path, suite: &BlockchainTest) -> Result<(), FixtureWriteError> {
    if is_binary_path(path) {
        let bytes = binary::to_vec(suite)?;
        if is_zstd_path(path) {
            write_zstd_bytes(File::create(path)?, &bytes)?;
        } else {
            fs::write(path, bytes)?;
        }
    } else {
        write_blockchain_json(File::create(path)?, suite, is_zstd_path(path))?;
    }
    Ok(())
}

fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    if is_zstd_path(path) {
        let file = File::open(path)?;
        let mut decoder = ZstdDecoder::new(file)?;
        let mut bytes = Vec::new();
        decoder.read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        fs::read(path)
    }
}

fn write_zstd_bytes(writer: impl Write, bytes: &[u8]) -> io::Result<()> {
    let writer = BufWriter::new(writer);
    let mut encoder = ZstdEncoder::new(writer, ZSTD_COMPRESSION_LEVEL)?;
    encoder.write_all(bytes)?;
    encoder.finish()?.flush()?;
    Ok(())
}

fn is_zstd_path(path: &Path) -> bool {
    has_extension(path, "zst")
}

pub(crate) fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|candidate| candidate == extension)
}

fn stem_has_extension(path: &Path, extension: &str) -> bool {
    path.file_stem().is_some_and(|stem| has_extension(Path::new(stem), extension))
}

fn write_blockchain_json(
    writer: impl Write,
    suite: &BlockchainTest,
    compressed: bool,
) -> Result<(), FixtureWriteError> {
    let mut writer = BufWriter::new(writer);
    if compressed {
        let mut encoder = ZstdEncoder::new(&mut writer, ZSTD_COMPRESSION_LEVEL)?;
        serde_json::to_writer(&mut encoder, suite)?;
        encoder.finish()?;
    } else {
        serde_json::to_writer(&mut writer, suite)?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingWriter {
        fail_write: bool,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write { Err(io::Error::other("write failed")) } else { Ok(bytes.len()) }
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    #[test]
    fn json_propagates_write_and_flush_errors() {
        let suite = BlockchainTest(Default::default());
        for compressed in [false, true] {
            for fail_write in [false, true] {
                let error = write_blockchain_json(FailingWriter { fail_write }, &suite, compressed)
                    .unwrap_err();
                assert!(
                    matches!(error, FixtureWriteError::Write(error) if error.kind() == io::ErrorKind::Other)
                );
            }
        }
    }

    #[test]
    fn zstd_bytes_propagates_write_and_flush_errors() {
        let suite = BlockchainTest(Default::default());
        let bytes = binary::to_vec(&suite).unwrap();
        for fail_write in [false, true] {
            let error = write_zstd_bytes(FailingWriter { fail_write }, &bytes).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
        }
    }

    #[test]
    fn json_roundtrip() {
        let suite = BlockchainTest(Default::default());
        for compressed in [false, true] {
            let mut output = Vec::new();
            write_blockchain_json(&mut output, &suite, compressed).unwrap();
            let json = if compressed {
                zstd::stream::decode_all(output.as_slice()).unwrap()
            } else {
                output
            };
            assert_eq!(json, b"{}");
        }
    }

    #[test]
    fn zstd_bytes_roundtrip() {
        let suite = BlockchainTest(Default::default());
        let bytes = binary::to_vec(&suite).unwrap();
        let mut output = Vec::new();
        write_zstd_bytes(&mut output, &bytes).unwrap();
        let decoded = zstd::stream::decode_all(output.as_slice()).unwrap();
        assert_eq!(decoded, bytes);
    }
}
