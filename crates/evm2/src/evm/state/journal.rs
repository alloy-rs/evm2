//! Revert journal and checkpoint types.

use super::{Account, StorageOverlay};
use crate::interpreter::Word;
use alloy_primitives::Address;

/// State checkpoint for reverting state changes.
#[allow(missing_copy_implementations)]
#[derive(Debug, Eq, PartialEq)]
pub struct StateCheckpoint {
    /// Revert journal length at the checkpoint.
    pub(super) journal_len: usize,
    /// Emitted log count at the checkpoint.
    pub(super) logs_len: usize,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        evm::{
            CacheDB,
            state::{AccountInfo, State},
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::Log;

    #[test]
    fn destruct_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x33; 20]);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        state.account_entry(&address, false).unwrap().mark_destructed();

        assert!(state.account_entry(&address, false).unwrap().is_destructed());
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.account_entry(&address, false).unwrap().is_destructed());
    }

    #[test]
    fn log_rolls_back_to_checkpoint() {
        use alloy_primitives::{Bytes, LogData};

        let mut state = State::new(CacheDB::default());
        let kept = Log {
            address: Address::from([0x44; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x01])),
        };
        let reverted = Log {
            address: Address::from([0x55; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x02])),
        };

        state.log(kept.clone());
        let checkpoint = state.checkpoint();
        state.log(reverted);

        assert_eq!(
            state.logs(),
            &[
                kept.clone(),
                Log {
                    address: Address::from([0x55; 20]),
                    data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x02])),
                }
            ]
        );
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.logs(), &[kept]);
    }

    #[test]
    fn spurious_dragon_rollback_preserves_precompile3_touch() {
        let precompile3 = Address::with_last_byte(3);
        let other = Address::with_last_byte(4);
        let mut database = CacheDB::default();
        database.insert_account_info(&precompile3, AccountInfo::default());
        database.insert_account_info(&other, AccountInfo::default());
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.account_entry(&precompile3, false).unwrap().touch();
        state.account_entry(&other, false).unwrap().touch();

        state.rollback(checkpoint, Version::base(SpecId::SPURIOUS_DRAGON).features);
        assert!(state.account_entry(&precompile3, false).unwrap().is_touched());
        assert!(!state.account_entry(&other, false).unwrap().is_touched());
    }

    #[test]
    fn non_revertible_warmth_is_not_journaled_or_rolled_back() {
        let base_account = Address::with_last_byte(0x10);
        let frame_account = Address::with_last_byte(0x11);
        let base_storage = Address::with_last_byte(0x12);
        let frame_storage = Address::with_last_byte(0x13);
        let key = Word::from(1);
        let mut state = State::new(CacheDB::default());

        state.prewarmset_mut().warm_account(&base_account);
        assert!(state.prewarmset_mut().warm_storage(&base_storage, &key));
        assert!(state.journal.is_empty());

        let checkpoint = state.checkpoint();
        assert!(state.warm_account(&frame_account));
        assert!(state.warm_storage(&frame_storage, &key));
        assert_eq!(state.journal.len(), 2);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.account_entry(&base_account, false).unwrap().is_warm());
        assert!(state.storage_slot_entry(&base_storage, key).is_warm());
        assert!(!state.account_entry(&frame_account, false).unwrap().is_warm());
        assert!(!state.storage_slot_entry(&frame_storage, key).is_warm());
    }

    #[test]
    fn warm_only_entries_do_not_emit_state_changes() {
        let account = Address::with_last_byte(0x14);
        let storage_account = Address::with_last_byte(0x15);
        let key = Word::from(1);
        let mut state = State::new(CacheDB::default());

        state.prewarmset_mut().warm_account(&account);
        assert!(state.prewarmset_mut().warm_storage(&storage_account, &key));

        let changes = state.build_state_changes();
        assert!(changes.is_empty());
        assert!(state.account_entry(&account, false).unwrap().is_warm());
        assert!(state.storage_slot_entry(&storage_account, key).is_warm());

        state.clear_transaction_state();
        assert!(!state.account_entry(&account, false).unwrap().is_warm());
        assert!(!state.storage_slot_entry(&storage_account, key).is_warm());
    }

    #[test]
    fn rollback_preserves_non_revertible_account_warmth_after_load() {
        let account = Address::with_last_byte(0x16);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        state.prewarmset_mut().warm_account(&account);
        let checkpoint = state.checkpoint();
        assert!(state.account_entry(&account, false).unwrap().exists());
        assert!(state.account_lookup(&account).is_some());

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        // Warmth is non-revertible (base warm set), so check it without re-loading the account,
        // which would otherwise repopulate the overlay the rollback just cleared.
        assert!(state.prewarmset().is_warm(&account));
        assert!(state.account_lookup(&account).is_none());
        assert!(state.build_state_changes().is_empty());
    }

    #[test]
    fn rollback_preserves_non_revertible_storage_warmth_after_write() {
        let account = Address::with_last_byte(0x17);
        let key = Word::from(1);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        assert!(state.prewarmset_mut().warm_storage(&account, &key));
        let checkpoint = state.checkpoint();
        state.storage_entry(&account).slot(key).write(Word::from(7), false).unwrap();
        assert_eq!(state.storage_lookup(&account, &key), Some(Word::from(7)));

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.storage_slot_entry(&account, key).is_warm());
        assert!(state.build_state_changes().is_empty());
    }

    #[test]
    fn rollback_reverts_storage_warmth_without_discarding_cached_value() {
        let account = Address::with_last_byte(0x18);
        let key = Word::from(1);
        let value = Word::from(9);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&account, &key, &value);
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        assert!(state.warm_storage(&account, &key));
        assert_eq!(state.storage_entry(&account).slot(key).load(false).unwrap(), value);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.storage_slot_entry(&account, key).is_warm());
        assert_eq!(state.storage_lookup(&account, &key), Some(value));
        assert!(state.build_state_changes().is_empty());

        assert!(state.warm_storage(&account, &key));
    }
}
