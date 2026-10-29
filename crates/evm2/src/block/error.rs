//! Block execution errors.

use crate::{DatabaseError, registry::HandlerError};
use alloc::{
    boxed::Box,
    string::{String, ToString},
};
use alloy_primitives::B256;
use core::{any::Any, error::Error};

/// A transaction validation error.
pub trait InvalidTxError: Error + Send + Sync + Any + 'static {
    /// Returns whether the transaction nonce is lower than expected.
    fn is_nonce_too_low(&self) -> bool {
        false
    }

    /// Returns whether the transaction gas limit is higher than allowed.
    fn is_gas_limit_too_high(&self) -> bool {
        false
    }

    /// Returns whether the transaction gas limit is lower than required.
    fn is_gas_limit_too_low(&self) -> bool {
        false
    }

    /// Returns the error as [`Any`], for callers that downcast it.
    fn as_any(&self) -> &(dyn Any + 'static);
}

/// A block that is invalid.
#[derive(Debug, thiserror::Error)]
pub enum BlockValidationError {
    /// A transaction is invalid.
    #[error("EVM reported invalid transaction ({hash}): {error}")]
    InvalidTx {
        /// Transaction hash.
        hash: B256,
        /// Validation error.
        error: Box<dyn InvalidTxError>,
    },
    /// A transaction failed for a reason other than validation.
    #[error("EVM execution failed: {error}")]
    EVM {
        /// Transaction hash.
        hash: B256,
        /// EVM error.
        error: Box<dyn Error + Send + Sync + 'static>,
    },
    /// Incrementing a balance after execution failed.
    #[error("incrementing balance in post execution failed")]
    IncrementBalanceFailed,
    /// A transaction's gas limit is more than the gas left in the block.
    #[error(
        "transaction gas limit {transaction_gas_limit} is more than blocks available gas {block_available_gas}"
    )]
    TransactionGasLimitMoreThanAvailableBlockGas {
        /// Transaction gas limit.
        transaction_gas_limit: u64,
        /// Gas left in the block.
        block_available_gas: u64,
    },
    /// A Cancun block has no EIP-4788 parent beacon block root.
    #[error("EIP-4788 parent beacon block root missing for active Cancun block")]
    MissingParentBeaconBlockRoot,
    /// A Cancun genesis block has a parent beacon block root other than zero.
    #[error(
        "the parent beacon block root is not zero for Cancun genesis block: {parent_beacon_block_root}"
    )]
    CancunGenesisParentBeaconBlockRootNotZero {
        /// Parent beacon block root.
        parent_beacon_block_root: B256,
    },
    /// The EIP-7002 withdrawal requests system call failed.
    #[error("failed to apply withdrawal requests contract call: {message}")]
    WithdrawalRequestsContractCall {
        /// Error message.
        message: String,
    },
    /// The EIP-7251 consolidation requests system call failed.
    #[error("failed to apply consolidation requests contract call: {message}")]
    ConsolidationRequestsContractCall {
        /// Error message.
        message: String,
    },
    /// Decoding EIP-6110 deposit requests from receipts failed.
    #[error("failed to decode deposit requests from receipts: {_0}")]
    DepositRequestDecode(String),
    /// The block's gas used exceeds its gas limit.
    #[error("block gas used exceeds block gas limit")]
    BlockGasExceeded,
    /// Any other validation error.
    #[error(transparent)]
    Other(Box<dyn Error + Send + Sync + 'static>),
}

impl BlockValidationError {
    /// Creates a [`Self::Other`] error.
    pub fn other<E: Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Other(Box::new(error))
    }

    /// Creates a [`Self::Other`] error from a message.
    pub fn msg(msg: impl core::fmt::Display) -> Self {
        Self::Other(msg.to_string().into())
    }
}

/// A block execution error.
#[derive(Debug, thiserror::Error)]
pub enum BlockExecutionError {
    /// The block is invalid.
    #[error(transparent)]
    Validation(#[from] BlockValidationError),
    /// Execution failed for a reason unrelated to the block's validity.
    #[error(transparent)]
    Internal(#[from] InternalBlockExecutionError),
}

impl BlockExecutionError {
    /// Creates an [`InternalBlockExecutionError::Other`] error.
    pub fn other<E: Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Internal(InternalBlockExecutionError::other(error))
    }

    /// Creates an [`InternalBlockExecutionError::Other`] error from a message.
    pub fn msg(msg: impl core::fmt::Display) -> Self {
        Self::Internal(InternalBlockExecutionError::msg(msg))
    }

    /// Returns the validation error, if this is one.
    pub const fn as_validation(&self) -> Option<&BlockValidationError> {
        match self {
            Self::Validation(err) => Some(err),
            Self::Internal(_) => None,
        }
    }

    /// Returns the internal error, if this is one.
    pub const fn as_internal(&self) -> Option<&InternalBlockExecutionError> {
        match self {
            Self::Internal(err) => Some(err),
            Self::Validation(_) => None,
        }
    }
}

/// A block execution error unrelated to the block's validity.
#[derive(Debug, thiserror::Error)]
pub enum InternalBlockExecutionError {
    /// The EVM failed while executing a transaction.
    #[error("internal EVM error occurred when executing transaction {hash}: {error}")]
    EVM {
        /// Transaction hash.
        hash: B256,
        /// EVM error.
        error: Box<dyn Error + Send + Sync + 'static>,
    },
    /// Any other internal error.
    #[error(transparent)]
    Other(Box<dyn Error + Send + Sync + 'static>),
}

impl InternalBlockExecutionError {
    /// Creates a [`Self::Other`] error.
    pub fn other<E: Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Other(Box::new(error))
    }

    /// Creates a [`Self::Other`] error from a message.
    pub fn msg(msg: impl core::fmt::Display) -> Self {
        Self::Other(msg.to_string().into())
    }
}

/// Converts a handler error that has no transaction hash into a block error.
pub fn map_handler_error(error: HandlerError) -> BlockExecutionError {
    match error {
        HandlerError::Database(error) => map_database_error(error),
        HandlerError::Fatal(_) | HandlerError::WrongTransactionType { .. } => {
            BlockExecutionError::other(error)
        }
        error => BlockValidationError::other(error).into(),
    }
}

/// Converts a database error into a block error, keeping fatal errors internal.
pub fn map_database_error(error: DatabaseError) -> BlockExecutionError {
    if error.is_fatal() {
        BlockExecutionError::other(error)
    } else {
        BlockValidationError::other(error).into()
    }
}
