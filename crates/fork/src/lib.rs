//! RPC forking database for evm2, modeled on Foundry's forking backend.
//!
//! [`SharedBackend`] implements [`DatabaseRef`](evm2::evm::DatabaseRef) on top of a shared
//! [`BlockchainDb`] cache, and asks a background [`BackendHandler`] to fetch whatever the cache
//! is missing from a JSON-RPC provider at a pinned block. Wrap it in a [`ForkDB`] to execute
//! transactions against the forked state while keeping every write local.
#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

use evm2::evm::{CacheDB, DbRef};

mod backend;
pub use backend::{BackendHandler, SharedBackend};

mod cache;
pub use cache::{BlockchainDb, BlockchainDbMeta, JsonBlockCacheData};

mod error;
pub use error::{ForkError, ForkResult};

/// A forked database: a local [`CacheDB`] overlay over a [`SharedBackend`].
///
/// Committed state stays in the overlay; the shared backend only ever holds remote state.
pub type ForkDB = CacheDB<DbRef<SharedBackend>>;
