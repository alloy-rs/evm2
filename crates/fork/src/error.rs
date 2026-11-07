//! Fork backend errors.

use alloy_primitives::{B256, U256};
use alloy_provider::transport::TransportError;
use std::sync::Arc;

/// Result of a fork backend operation.
pub type ForkResult<T> = Result<T, ForkError>;

/// Errors returned by the fork backend.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ForkError {
    /// The RPC request failed.
    #[error("RPC request failed: {0}")]
    Transport(Arc<TransportError>),
    /// The remote node does not know the requested block.
    #[error("block {0} not found on the remote node")]
    BlockNotFound(u64),
    /// A `BLOCKHASH` number does not fit in a `u64`.
    #[error("block number {0} does not fit in a u64")]
    InvalidBlockNumber(U256),
    /// Bytecode was requested by hash before any account with that code was loaded.
    ///
    /// JSON-RPC cannot look up code by hash, so code is only known once an account carrying it
    /// was fetched.
    #[error("bytecode for code hash {0} is not cached and cannot be fetched by hash over RPC")]
    MissingCode(B256),
    /// The background handler is no longer running.
    #[error("fork backend handler has shut down")]
    BackendShutdown,
}

impl From<TransportError> for ForkError {
    #[inline]
    fn from(error: TransportError) -> Self {
        Self::Transport(Arc::new(error))
    }
}
