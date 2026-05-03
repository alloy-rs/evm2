use alloy_primitives::{B256, Bytes, U256};
use evm2::registry::HandlerError;
use std::{io, path::PathBuf};
use thiserror::Error;

/// State test runner error.
#[derive(Debug, Error)]
#[error("Path: {path}\nName: {name}\nError: {kind}")]
pub(crate) struct TestError {
    /// Test path.
    pub(crate) path: String,
    /// Test name.
    pub(crate) name: String,
    /// Error kind.
    pub(crate) kind: TestErrorKind,
}

impl TestError {
    /// Creates an error for a path-level failure.
    pub(crate) fn path(path: impl Into<PathBuf>, kind: TestErrorKind) -> Self {
        Self { path: path.into().display().to_string(), name: "Path validation".to_string(), kind }
    }

    /// Creates an error for an unknown test name.
    pub(crate) fn unknown(path: impl Into<PathBuf>, kind: TestErrorKind) -> Self {
        Self { path: path.into().display().to_string(), name: "Unknown".to_string(), kind }
    }

    /// Creates an error for a named test case.
    pub(crate) fn case(
        path: impl Into<PathBuf>,
        name: impl Into<String>,
        kind: TestErrorKind,
    ) -> Self {
        Self { path: path.into().display().to_string(), name: name.into(), kind }
    }
}

/// Specific kind of error that occurred during test execution.
#[derive(Debug, Error)]
pub(crate) enum TestErrorKind {
    /// Invalid test path.
    #[error("path does not exist")]
    InvalidPath,
    /// No JSON tests were found.
    #[error("no JSON test files found in path")]
    NoJsonFiles,
    /// Directory traversal failed.
    #[error("walk error: {0}")]
    Walk(#[from] walkdir::Error),
    /// File read failed.
    #[error("read error: {0}")]
    Read(#[from] io::Error),
    /// JSON decoding failed.
    #[error(transparent)]
    SerdeDeserialize(#[from] serde_json::Error),
    /// Logs root mismatch.
    #[error("logs root mismatch: got {got}, expected {expected}")]
    LogsRootMismatch {
        /// Actual logs root.
        got: B256,
        /// Expected logs root.
        expected: B256,
    },
    /// State root mismatch.
    #[error("state root mismatch: got {got}, expected {expected}")]
    StateRootMismatch {
        /// Actual state root.
        got: B256,
        /// Expected state root.
        expected: B256,
    },
    /// Sender could not be recovered.
    #[error("unknown private key: {0:?}")]
    UnknownPrivateKey(B256),
    /// Dynamic-fee transaction max fee is lower than the block base fee.
    #[error(
        "max fee per gas is lower than block base fee: max_fee_per_gas={max_fee_per_gas}, base_fee={base_fee}"
    )]
    FeeCapLessThanBaseFee {
        /// Transaction fee cap.
        max_fee_per_gas: U256,
        /// Block base fee.
        base_fee: U256,
    },
    /// Unexpected exception status.
    #[error("unexpected exception: got {got_exception:?}, expected {expected_exception:?}")]
    UnexpectedException {
        /// Expected exception.
        expected_exception: Option<String>,
        /// Actual exception.
        got_exception: Option<String>,
    },
    /// Output mismatch.
    #[error("unexpected output: got {got_output:?}, expected {expected_output:?}")]
    UnexpectedOutput {
        /// Expected output.
        expected_output: Option<Bytes>,
        /// Actual output.
        got_output: Option<Bytes>,
    },
    /// Numeric value overflowed the target type.
    #[error("value overflows {0}")]
    Overflow(&'static str),
    /// Transaction part index was invalid.
    #[error("bad transaction index: {0}")]
    BadIndex(&'static str),
    /// EVM execution failed.
    #[error(transparent)]
    Evm(#[from] HandlerError),
}
