use alloy_primitives::B256;
use evm2::registry::HandlerError;
use std::path::PathBuf;

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("expected manifest.json or replay .bin files under {path}")]
    MissingInput { path: PathBuf },
    #[error("expected replay block file to use .bin extension: {path}")]
    UnsupportedInputFile { path: PathBuf },
    #[error("failed to list replay block files under {path}")]
    ListBlockFiles {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read replay block file entry under {path}")]
    ReadBlockFileEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read replay block from {path}")]
    ReadBlock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode replay block from {path}")]
    DecodeBlock {
        path: PathBuf,
        #[source]
        source: bincode::Error,
    },
    #[error("failed to read replay manifest from {path}")]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode replay manifest from {path}")]
    DecodeManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported replay manifest version {actual}, expected {expected}")]
    UnsupportedManifestVersion { actual: u32, expected: u32 },
    #[error(
        "replay failed after {index} successful blocks at block {block_number} ({block_hash}) from {path}"
    )]
    BlockContext {
        index: usize,
        block_number: u64,
        block_hash: B256,
        path: PathBuf,
        #[source]
        source: Box<Self>,
    },
    #[error("execute failed after {completed} blocks at block {block_number} ({block_hash})")]
    ExecuteContext {
        completed: usize,
        block_number: u64,
        block_hash: B256,
        #[source]
        source: Box<Self>,
    },
    #[error("failed to decode raw block RLP")]
    DecodeRawBlock {
        #[source]
        source: alloy_rlp::Error,
    },
    #[error("trailing bytes after raw block RLP")]
    TrailingRawBlockRlp,
    #[error("block hash mismatch: expected {expected}, got {actual}")]
    BlockHashMismatch { expected: B256, actual: B256 },
    #[error("block number mismatch: expected {expected}, got {actual}")]
    BlockNumberMismatch { expected: u64, actual: u64 },
    #[error("parent hash mismatch: expected {expected}, got {actual}")]
    ParentHashMismatch { expected: B256, actual: B256 },
    #[error("transaction count mismatch: expected {expected}, got {actual}")]
    TransactionCountMismatch { expected: usize, actual: usize },
    #[error("transaction #{index} hash mismatch: expected {expected}, got {actual}")]
    TransactionHashMismatch { index: usize, expected: B256, actual: B256 },
    #[error("transaction #{index} encoded bytes do not match raw block")]
    TransactionEncodingMismatch { index: usize },
    #[error("history storage system call failed: {stop}")]
    HistoryStorageSystemCall { stop: String },
    #[error("beacon roots system call failed: {stop}")]
    BeaconRootsSystemCall { stop: String },
    #[error("block {block_number} requires base fee for post-London replay")]
    MissingBaseFee { block_number: u64 },
    #[error("block {block_number} requires excess blob gas for post-Cancun replay")]
    MissingExcessBlobGas { block_number: u64 },
    #[error("block {block_number} ({block_hash}) transaction #{index} ({tx_hash}) failed")]
    TransactionExecution {
        block_number: u64,
        block_hash: B256,
        index: usize,
        tx_hash: B256,
        #[source]
        source: Box<HandlerError>,
    },
    #[error(
        "block {block_number} ({block_hash}) gas used mismatch: got {actual}, expected {expected}"
    )]
    GasUsedMismatch { block_number: u64, block_hash: B256, actual: u128, expected: u64 },
}
