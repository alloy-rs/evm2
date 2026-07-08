//! Block Access List (BAL) data structures for efficient state access in blockchain execution.
//!
//! This module provides types for managing Block Access Lists, which optimize state access
//! by pre-computing and organizing data that will be accessed during block execution.
//!
//! ## Key Types
//!
//! - [`BlockAccessIndex`]: block access index
//! - [`Bal`]: Main BAL structure containing a map of accounts
//! - [`BalWrites<T>`]: Array of (index, value) pairs representing sequential writes to a state item
//! - [`AccountBal`]: Complete BAL structure for an account (balance, nonce, code, and storage)
//! - [`AccountInfoBal`]: Account info BAL data (nonce, balance, code)
//! - [`StorageBal`]: Storage-level BAL data for an account
//! - [`BalContext`]: attached read BAL plus optional builder, carried by the database wrapper
//! - [`BalError`]: lookup failures against an attached BAL

pub mod account;
pub mod alloy;
pub mod bal_context;
pub mod error;
pub mod list;
pub mod writes;

pub use account::{AccountBal, AccountInfoBal, StorageBal};
pub use alloy_eip7928::BlockAccessIndex;
pub use error::BalError;
pub use list::Bal;
pub use writes::BalWrites;
