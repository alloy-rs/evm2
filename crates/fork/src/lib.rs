//! RPC forking database for evm2, modeled on Foundry's forking backend.
//!
//! [`SharedBackend`] implements [`DatabaseRef`](evm2::evm::DatabaseRef) on top of a shared
//! [`BlockchainDb`] cache, and asks a background [`BackendHandler`] to fetch whatever the cache
//! is missing from a JSON-RPC provider at a pinned block. Wrap it in a [`ForkDB`] to execute
//! transactions against the forked state while keeping every write local.
#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

use alloy_provider::network::Ethereum;
use evm2::evm::{CacheDB, DbRef};

mod backend;
pub use backend::{AccountFetchPolicy, BackendHandler, BlockingMode, SharedBackend};

pub mod bal;

mod cache;
pub use cache::{BlockchainDb, BlockchainDbMeta, JsonBlockCacheData};

mod database;
pub use database::*;

pub mod env;
pub use env::{ForkEnv, fork_env};

mod error;
pub use error::{ForkError, ForkResult, NON_ARCHIVE_NODE_WARNING};

mod multi;
pub use multi::*;

mod prefetch;

/// A forked database: a local [`CacheDB`] overlay over a [`SharedBackend`].
///
/// Committed state stays in the overlay; the shared backend only ever holds remote state.
pub type ForkDB<N = Ethereum> = CacheDB<DbRef<SharedBackend<N>>>;
