//! Fork backend errors.

use alloy_eips::BlockId;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::transport::TransportError;
use std::{error::Error, sync::Arc};

/// Result of a fork backend operation.
pub type ForkResult<T> = Result<T, ForkError>;

/// Logged when an error suggests the fork reads an old block from a non-archive node.
pub const NON_ARCHIVE_NODE_WARNING: &str = "\
It looks like you're trying to fork from an older block with a non-archive node which is not \
supported. Please try to change your RPC url to an archive node if the issue persists.";

/// Errors returned by the fork backend.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ForkError {
    /// Bytecode was requested by hash before any account with that code was loaded.
    ///
    /// JSON-RPC cannot look up code by hash, so code is only known once an account carrying it
    /// was fetched.
    #[error("missing bytecode for code hash {0}")]
    MissingCode(B256),
    /// Fetching an account failed.
    #[error("failed to get account for {0}: {1}")]
    GetAccount(Address, Arc<TransportError>),
    /// Fetching a storage slot failed.
    #[error("failed to get storage for {0} at {1}: {2}")]
    GetStorage(Address, U256, Arc<TransportError>),
    /// Fetching a block hash failed.
    #[error("failed to get block hash for {0}: {1}")]
    GetBlockHash(u64, Arc<TransportError>),
    /// Fetching the chain ID failed.
    #[error("failed to get chain id: {0}")]
    GetChainId(Arc<TransportError>),
    /// Predicting a block's state with `eth_createAccessList` failed.
    #[error("failed to create access lists: {0}")]
    CreateAccessList(Arc<TransportError>),
    /// Fetching a full block failed.
    #[error("failed to get full block for {0}: {1}")]
    GetFullBlock(BlockId, Arc<TransportError>),
    /// The remote node does not know the requested block.
    #[error("block {0} does not exist")]
    BlockNotFound(BlockId),
    /// Fetching a transaction failed.
    #[error("failed to get transaction {0}: {1}")]
    GetTransaction(B256, Arc<TransportError>),
    /// The remote node does not know the requested transaction.
    #[error("transaction {0} does not exist")]
    TransactionNotFound(B256),
    /// A `BLOCKHASH` number does not fit in a `u64`.
    #[error("block number {0} does not fit in a u64")]
    InvalidBlockNumber(U256),
    /// A request sent with [`SharedBackend::do_any_request`](crate::SharedBackend::do_any_request)
    /// failed.
    #[error("failed to process AnyRequest: {0}")]
    AnyRequest(Arc<dyn Error + Send + Sync>),
    /// The background handler is no longer running.
    #[error("fork backend handler has shut down")]
    BackendShutdown,
    /// Connecting to a fork endpoint failed.
    #[error("failed to connect to fork endpoint: {0}")]
    Connect(Arc<TransportError>),
    /// No fork has the given identifier.
    #[error("no fork with id {0}")]
    UnknownFork(String),
}

impl ForkError {
    fn rpc_error(&self) -> Option<&dyn Error> {
        match self {
            Self::GetAccount(_, err)
            | Self::GetStorage(_, _, err)
            | Self::GetBlockHash(_, err)
            | Self::GetFullBlock(_, err)
            | Self::GetTransaction(_, err)
            | Self::GetChainId(err)
            | Self::CreateAccessList(err)
            | Self::Connect(err) => Some(&**err),
            Self::AnyRequest(err) => Some(&**err),
            // Enumerate explicitly so new variants are classified.
            Self::MissingCode(_)
            | Self::BlockNotFound(_)
            | Self::TransactionNotFound(_)
            | Self::InvalidBlockNumber(_)
            | Self::BackendShutdown
            | Self::UnknownFork(_) => None,
        }
    }

    /// Whether the error is possibly caused by forking an old block from a non-archive node.
    pub fn is_possibly_non_archive_node_error(&self) -> bool {
        self.rpc_error()
            .is_some_and(|err| err.to_string().to_lowercase().contains("missing trie node"))
    }
}
