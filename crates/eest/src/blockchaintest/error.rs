use super::types::{BlobFork, ForkSpec};
use crate::{exception::Exception, execution::ExecutionResourceError, tx::TxBuildError};
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
    /// The runner does not support the test's fork.
    #[error("unsupported fork: {0:?}")]
    UnsupportedFork(ForkSpec),
    /// The test's chain configuration has no blob parameters for the active fork.
    #[error("blob schedule has no entry for {0:?}")]
    MissingBlobSchedule(BlobFork),
    /// The test's blob schedule has a zero base fee update fraction for the active fork.
    #[error("blob schedule entry for {0:?} has a zero base fee update fraction")]
    ZeroBlobUpdateFraction(BlobFork),
    /// Sender is required when signatures are not validated.
    #[error("transaction sender is required")]
    MissingSender,
    /// Transaction could not be built, decoded, or its sender recovered.
    #[error(transparent)]
    Transaction(#[from] TxBuildError),
    /// Sender recovered from the signature differs from the fixture's sender.
    #[error("sender mismatch: recovered {recovered}, fixture {expected}")]
    SenderMismatch {
        /// Sender recovered from the signature.
        recovered: alloy_primitives::Address,
        /// Sender declared by the fixture.
        expected: alloy_primitives::Address,
    },
    /// An ommer is not old enough, or is too old, to receive a block reward.
    #[error("invalid ommer age: block {block_number}, ommer {ommer_number}")]
    InvalidOmmerAge {
        /// Canonical block number.
        block_number: alloy_primitives::U256,
        /// Ommer block number.
        ommer_number: alloy_primitives::U256,
    },
    /// EVM execution failed.
    #[error(transparent)]
    Evm(#[from] HandlerError),
    /// EVM execution unexpectedly failed.
    #[error("unexpected execution failure: {0}")]
    UnexpectedFailure(String),
    /// The block breaks a validity rule execution-specs names.
    #[error("{reason}")]
    InvalidBlock {
        /// Exception execution-specs raises for the broken rule.
        exception: Exception,
        /// Human-readable description of the broken rule.
        reason: String,
    },
    /// The block was rejected for a reason its fixture did not expect.
    #[error("unexpected exception: got {got}, expected {expected}")]
    UnexpectedException {
        /// Exceptions the fixture expects, separated by `|`.
        expected: String,
        /// Actual error, led by its exception name when it has one.
        got: String,
    },
    /// EVM execution unexpectedly succeeded.
    #[error("execution succeeded, but expected exception: {0}")]
    UnexpectedSuccess(String),
    /// A system call failed.
    #[error("system call failed: {0}")]
    SystemCall(&'static str),
    /// A system contract has no code at the end of block execution.
    #[error("{0} system contract has no code")]
    SystemContractEmpty(&'static str),
    /// Block gas used in the header does not match the executed transactions.
    #[error("block gas used mismatch: header {expected}, computed {actual}")]
    BlockGasUsedMismatch {
        /// Gas used declared in the block header.
        expected: u64,
        /// Gas used computed from executing the block's transactions.
        actual: u64,
    },
    /// Block header does not link to the previously accepted block.
    #[error("parent block hash mismatch: header {got}, expected {expected}")]
    ParentBlockHashMismatch {
        /// Parent hash declared in the block header.
        got: alloy_primitives::B256,
        /// Hash of the previously accepted block.
        expected: alloy_primitives::B256,
    },
    /// Final accepted block hash does not match the fixture's last block hash.
    #[error("last block hash mismatch: got {got}, expected {expected}")]
    LastBlockHashMismatch {
        /// Hash of the final accepted block.
        got: alloy_primitives::B256,
        /// Last block hash declared by the fixture.
        expected: alloy_primitives::B256,
    },
    /// Recomputed post-block state root does not match the block header.
    #[error("state root mismatch: got {got}, expected {expected}")]
    StateRootMismatch {
        /// State root recomputed from the post-block database.
        got: alloy_primitives::B256,
        /// State root declared in the block header.
        expected: alloy_primitives::B256,
    },
    /// Recomputed receipt trie root does not match the block header.
    #[error("receipt root mismatch: got {got}, expected {expected}")]
    ReceiptRootMismatch {
        /// Receipt trie root recomputed from transaction execution results.
        got: alloy_primitives::B256,
        /// Receipt trie root declared in the block header.
        expected: alloy_primitives::B256,
    },
    /// Execution resource initialization or compilation failed.
    #[error(transparent)]
    ExecutionResource(#[from] ExecutionResourceError),
    /// The built EIP-7928 block access list does not match the block header's.
    #[error("block access list mismatch at block {block_index}:{details}")]
    BlockAccessListMismatch {
        /// Index of the block whose access list mismatched.
        block_index: usize,
        /// Human-readable summary of the differing accounts.
        details: String,
    },
}

impl TestErrorKind {
    /// Returns the exception execution-specs raises for a block with this error, if it is a
    /// block or transaction validity failure.
    pub(crate) fn exception(&self) -> Option<Exception> {
        match self {
            Self::Transaction(error) => error.exception(),
            Self::Evm(error) => Exception::from_handler_error(error),
            Self::InvalidBlock { exception, .. } => Some(*exception),
            Self::SystemCall(_) => Some(Exception::SystemContractCallFailed),
            Self::SystemContractEmpty(_) => Some(Exception::SystemContractEmpty),
            Self::BlockGasUsedMismatch { .. } => Some(Exception::InvalidGasUsed),
            Self::BlockAccessListMismatch { .. } => Some(Exception::InvalidBlockAccessList),
            Self::ParentBlockHashMismatch { got, .. } if got.is_zero() => {
                Some(Exception::UnknownParentZero)
            }
            Self::ParentBlockHashMismatch { .. } => Some(Exception::UnknownParent),
            Self::ReceiptRootMismatch { .. } => Some(Exception::InvalidReceiptsRoot),
            _ => None,
        }
    }
}
