//! Revert journal entries and checkpoints.

use super::{Account, StorageOverlay};
use crate::interpreter::Word;
use alloy_primitives::Address;

/// State checkpoint for reverting state changes.
#[allow(missing_copy_implementations)]
#[derive(Debug, Eq, PartialEq)]
pub struct StateCheckpoint {
    /// Revert journal length at the checkpoint.
    pub(crate) journal_len: usize,
    /// Emitted log count at the checkpoint.
    pub(crate) logs_len: usize,
}

/// Compact journal entry for reverting state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JournalEntry {
    /// Account current value changed.
    AccountChange {
        /// Account address.
        address: Address,
        /// Previous current account value.
        previous: Option<Account>,
    },
    /// Account overlay entry was inserted.
    AccountInserted {
        /// Account address.
        address: Address,
    },
    /// Account was touched.
    Touch {
        /// Account address.
        address: Address,
    },
    /// Account was self-destructed.
    SelfDestruct {
        /// Account address.
        address: Address,
    },
    /// Persistent storage changed.
    StorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous current storage value.
        previous: Word,
    },
    /// Persistent storage slot overlay was inserted.
    StorageInserted {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
    },
    /// Account storage wipe flag changed.
    StorageWipe {
        /// Account address.
        address: Address,
        /// Previous storage overlay.
        previous: Option<StorageOverlay>,
    },
    /// Transient storage changed.
    TransientStorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous transient storage value.
        previous: Option<Word>,
    },
    /// Account was warmed by EIP-2929 access tracking.
    AccountWarmed {
        /// Account address.
        address: Address,
    },
    /// Storage slot was warmed by EIP-2929 access tracking.
    StorageWarmed {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
    },
}
