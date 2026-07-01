use crate::{execution::ExecutionResourceError, tx::TxBuildError};
use evm2::registry::HandlerError;
use std::{io, path::PathBuf};
use thiserror::Error;

/// Blockchain test runner error.
#[derive(Debug, Error)]
#[error("Path: {path}\nName: {name}\nError: {kind}")]
pub struct TestError {
    /// Test path.
    pub(crate) path: String,
    /// Test name.
    pub(crate) name: String,
    /// Error kind.
    pub(crate) kind: TestErrorKind,
}

impl TestError {
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
    /// File read failed.
    #[error("read error: {0}")]
    Read(#[from] io::Error),
    /// JSON decoding failed.
    #[error(transparent)]
    SerdeDeserialize(#[from] serde_json::Error),
    /// Fixture decoding failed.
    #[error(transparent)]
    FixtureRead(#[from] crate::fixture_io::FixtureReadError),
    /// Numeric value overflowed the target type.
    #[error("value overflows {0}")]
    Overflow(&'static str),
    /// Sender is required in blockchain tests.
    #[error("transaction sender is required")]
    MissingSender,
    /// Transaction request could not be converted to a consensus transaction.
    #[error("could not build consensus transaction: {0}")]
    BuildTransaction(String),
    /// EVM execution failed.
    #[error(transparent)]
    Evm(#[from] HandlerError),
    /// EVM execution unexpectedly failed.
    #[error("unexpected execution failure: {0}")]
    UnexpectedFailure(String),
    /// EVM execution unexpectedly succeeded.
    #[error("execution succeeded, but expected exception: {0}")]
    UnexpectedSuccess(String),
    /// A system call failed.
    #[error("system call failed: {0}")]
    SystemCall(&'static str),
    /// Block gas used in the header does not match the executed transactions.
    #[error("block gas used mismatch: header {expected}, computed {actual}")]
    BlockGasUsedMismatch {
        /// Gas used declared in the block header.
        expected: u64,
        /// Gas used computed from executing the block's transactions.
        actual: u64,
    },
    /// Execution resource initialization failed.
    #[error(transparent)]
    ExecutionResource(#[from] ExecutionResourceError),
}

impl From<TxBuildError> for TestErrorKind {
    fn from(error: TxBuildError) -> Self {
        match error {
            TxBuildError::Overflow(name) => Self::Overflow(name),
            TxBuildError::BuildTransaction(error) => Self::BuildTransaction(error),
            TxBuildError::SerdeDeserialize(error) => Self::SerdeDeserialize(error),
        }
    }
}
