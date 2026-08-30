//! Errors returned when importing a decoded EIP-7928 block access list.

use super::BlockAccessIndex;
use crate::bytecode::BytecodeDecodeError;
use alloy_primitives::{Address, U256};
use thiserror::Error;

/// A change list within an EIP-7928 account entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalChangeKind {
    /// Storage changes for one slot.
    Storage,
    /// Account balance changes.
    Balance,
    /// Account nonce changes.
    Nonce,
    /// Account code changes.
    Code,
}

impl core::fmt::Display for BalChangeKind {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Storage => "storage",
            Self::Balance => "balance",
            Self::Nonce => "nonce",
            Self::Code => "code",
        })
    }
}

/// Error returned when decoded EIP-7928 data is not canonical or cannot be imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BalDecodeError {
    /// A code change contains malformed bytecode.
    #[error(transparent)]
    Bytecode(#[from] BytecodeDecodeError),
    /// The same account occurs more than once.
    #[error("account {address} occurs more than once in the BAL")]
    DuplicateAccount {
        /// Duplicated account address.
        address: Address,
    },
    /// Account entries are not ordered lexicographically by address.
    #[error("BAL account {address} appears after {previous}, violating canonical order")]
    AccountsOutOfOrder {
        /// Previous account address.
        previous: Address,
        /// Out-of-order account address.
        address: Address,
    },
    /// The same storage key occurs more than once or in both storage lists.
    #[error("storage slot {slot:#x} occurs more than once for account {address}")]
    DuplicateStorageKey {
        /// Account containing the duplicated key.
        address: Address,
        /// Duplicated storage key.
        slot: U256,
    },
    /// Storage keys in one of the account's lists are not in canonical order.
    #[error(
        "storage slot {slot:#x} appears after {previous:#x} for account {address}, violating canonical order"
    )]
    StorageKeysOutOfOrder {
        /// Account containing the out-of-order key.
        address: Address,
        /// Previous storage key.
        previous: U256,
        /// Out-of-order storage key.
        slot: U256,
    },
    /// A `SlotChanges` entry has no changes.
    #[error("storage slot {slot:#x} has an empty change list for account {address}")]
    EmptyStorageChanges {
        /// Account containing the empty entry.
        address: Address,
        /// Storage key with no changes.
        slot: U256,
    },
    /// The same block access index occurs more than once in one change list.
    #[error(
        "block access index {index} occurs more than once in a {kind} change list for account {address}"
    )]
    DuplicateBlockAccessIndex {
        /// Account containing the change list.
        address: Address,
        /// Kind of change list.
        kind: BalChangeKind,
        /// Duplicated block access index.
        index: BlockAccessIndex,
    },
    /// A change list is not ordered by block access index.
    #[error(
        "block access index {index} appears after {previous} in a {kind} change list for account {address}"
    )]
    ChangeIndicesOutOfOrder {
        /// Account containing the change list.
        address: Address,
        /// Kind of change list.
        kind: BalChangeKind,
        /// Previous block access index.
        previous: BlockAccessIndex,
        /// Out-of-order block access index.
        index: BlockAccessIndex,
    },
    /// A block access index does not fit the EIP-7928 `uint32` representation.
    #[error(
        "block access index {index} in a {kind} change list for account {address} exceeds uint32"
    )]
    BlockAccessIndexOutOfRange {
        /// Account containing the change list.
        address: Address,
        /// Kind of change list.
        kind: BalChangeKind,
        /// Invalid block access index.
        index: BlockAccessIndex,
    },
    /// A block access index is greater than the block's post-execution index.
    #[error(
        "block access index {index} in a {kind} change list for account {address} exceeds the block maximum {max}"
    )]
    BlockAccessIndexExceedsBlock {
        /// Account containing the change list.
        address: Address,
        /// Kind of change list.
        kind: BalChangeKind,
        /// Invalid block access index.
        index: BlockAccessIndex,
        /// Post-execution index for the block.
        max: BlockAccessIndex,
    },
}
