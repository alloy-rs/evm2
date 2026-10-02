//! Revert journal and checkpoint types.

use super::AccountInfo;
use crate::interpreter::Word;
use alloy_primitives::Address;

/// State checkpoint for reverting state changes.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateCheckpoint {
    /// Revert journal length at the checkpoint.
    pub(crate) journal_len: usize,
    /// Emitted log count at the checkpoint.
    pub(crate) logs_len: usize,
}

impl StateCheckpoint {
    /// Creates a checkpoint from journal and log cursors.
    #[inline]
    pub const fn new(journal_len: usize, logs_len: usize) -> Self {
        Self { journal_len, logs_len }
    }

    /// Returns the revert-journal cursor captured by this checkpoint.
    #[inline]
    pub const fn journal_len(&self) -> usize {
        self.journal_len
    }

    /// Returns the emitted-log cursor captured by this checkpoint.
    #[inline]
    pub const fn logs_len(&self) -> usize {
        self.logs_len
    }
}

/// Compact journal entry for reverting state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JournalEntry {
    /// Account overlay snapshot recorded before the first mutation made through an
    /// [`AccountHandle`](super::AccountHandle), reverting the present account value and all
    /// per-account flags in one entry.
    AccountChange {
        /// Account address.
        address: Address,
        /// Previous present account value.
        previous: Option<AccountInfo>,
        /// Previous warm flag.
        previous_is_warm: bool,
        /// Previous touched flag.
        previous_is_touched: bool,
        /// Previous self-destructed flag.
        previous_is_destroyed: bool,
        /// Previous created-in-transaction flag.
        previous_just_created: bool,
        /// Previous code-changed flag.
        previous_code_changed: bool,
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
    /// Transient storage changed.
    TransientStorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous transient storage value.
        previous: Option<Word>,
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
        state.account(&address).unwrap().mark_destructed();

        assert!(state.account(&address).unwrap().is_destructed());
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.account(&address).unwrap().is_destructed());
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
        state.account(&precompile3).unwrap().touch();
        state.account(&other).unwrap().touch();

        state.rollback(checkpoint, Version::base(SpecId::SPURIOUS_DRAGON).features);
        assert!(state.account(&precompile3).unwrap().is_touched());
        assert!(!state.account(&other).unwrap().is_touched());
    }

    #[test]
    fn non_revertible_warmth_is_not_journaled_or_rolled_back() {
        let base_account = Address::with_last_byte(0x10);
        let frame_account = Address::with_last_byte(0x11);
        let base_storage = Address::with_last_byte(0x12);
        let frame_storage = Address::with_last_byte(0x13);
        let key = Word::from(1);
        let mut state = State::new(CacheDB::default());

        state.prewarm(&base_account);
        state.prewarm_storage_slot(&base_storage, key);
        assert!(state.journal.is_empty());

        let checkpoint = state.checkpoint();
        assert!(state.account(&frame_account).unwrap().warm());
        assert!(state.storage_slot(&frame_storage, key).unwrap().warm());
        // The load itself is an un-journaled read cache; warming the frame account records an
        // AccountChange and warming the slot records StorageWarmed: two revertible entries in
        // total.
        assert_eq!(state.journal.len(), 2);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.account(&base_account).unwrap().is_warm());
        assert!(state.storage_slot(&base_storage, key).unwrap().is_warm());
        assert!(!state.account(&frame_account).unwrap().is_warm());
        assert!(!state.storage_slot(&frame_storage, key).unwrap().is_warm());
    }

    #[test]
    fn warm_only_entries_do_not_emit_state_changes() {
        let account = Address::with_last_byte(0x14);
        let storage_account = Address::with_last_byte(0x15);
        let key = Word::from(1);
        let mut state = State::new(CacheDB::default());

        state.prewarm(&account);
        state.prewarm_storage_slot(&storage_account, key);

        let pending = state.take_pending_state();
        assert!(pending.is_empty());
        assert!(state.account(&account).unwrap().is_warm());
        assert!(state.storage_slot(&storage_account, key).unwrap().is_warm());

        state.clear_transaction_state();
        assert!(!state.account(&account).unwrap().is_warm());
        assert!(!state.storage_slot(&storage_account, key).unwrap().is_warm());
    }

    #[test]
    fn rollback_preserves_non_revertible_account_warmth_after_load() {
        let account = Address::with_last_byte(0x16);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        state.prewarm(&account);
        let checkpoint = state.checkpoint();
        assert!(state.account(&account).unwrap().exists());
        assert!(state.account(&account).unwrap().get().is_some());

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        // The load is an un-journaled read cache, so it survives rollback. Warmth comes from the
        // non-revertible base warm set, and the unchanged cached entry emits no state change.
        assert!(state.prewarm_set().is_warm(&account));
        assert!(state.account(&account).unwrap().is_warm());
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn rollback_preserves_non_revertible_storage_warmth_after_write() {
        let account = Address::with_last_byte(0x17);
        let key = Word::from(1);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        state.prewarm_storage_slot(&account, key);
        let checkpoint = state.checkpoint();
        state.storage(&account).into_slot(key).unwrap().write(Word::from(7));
        assert_eq!(state.storage_slot(&account, key).unwrap().current(), Word::from(7));

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.storage_slot(&account, key).unwrap().is_warm());
        assert!(!state.take_pending_state().is_changed());
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
        assert!(state.storage_slot(&account, key).unwrap().warm());
        assert_eq!(state.storage(&account).into_slot(key).unwrap().current(), value);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.storage_slot(&account, key).unwrap().is_warm());
        assert_eq!(state.storage_slot(&account, key).unwrap().current(), value);
        assert!(!state.take_pending_state().is_changed());

        assert!(state.storage_slot(&account, key).unwrap().warm());
    }

    #[test]
    fn consecutive_account_snapshots_keep_the_earliest_entry() {
        let address = Address::with_last_byte(0x20);
        let mut state = State::new(CacheDB::default());
        let checkpoint = state.checkpoint();
        state.account(&address).unwrap().set_balance(Word::from(10));
        state.account(&address).unwrap().set_nonce(2);
        state.account(&address).unwrap().warm();
        state.account(&address).unwrap().mark_created();
        state.account(&address).unwrap().mark_destructed();
        assert_eq!(state.journal.len(), 1);

        state.rollback(checkpoint, Version::base(SpecId::CANCUN).features);
        let account = state.account(&address).unwrap();
        assert!(!account.exists());
        assert!(!account.is_touched());
        assert!(!account.is_warm());
        assert!(!account.is_created());
        assert!(!account.is_destructed());
    }

    #[test]
    fn account_snapshots_do_not_skip_intervening_journal_entries() {
        let address = Address::with_last_byte(0x21);
        let other = Address::with_last_byte(0x22);
        for barrier in 0..3 {
            let mut state = State::new(CacheDB::default());
            let checkpoint = state.checkpoint();
            state.account(&address).unwrap().set_balance(Word::from(10));
            match barrier {
                0 => state.account(&other).unwrap().set_balance(Word::from(20)),
                1 => state.tstore(&address, &Word::ZERO, &Word::from(20)),
                _ => {
                    state.storage_slot(&address, Word::ZERO).unwrap().write(Word::from(20));
                }
            }
            let entries = state.journal.len();
            state.account(&address).unwrap().set_balance(Word::from(30));
            state.account(&address).unwrap().set_nonce(3);
            assert_eq!(state.journal.len(), entries + 1);

            state.rollback(checkpoint, Version::base(SpecId::CANCUN).features);
            assert!(!state.account(&address).unwrap().exists());
            assert!(!state.account(&other).unwrap().exists());
            assert_eq!(state.tload(&address, &Word::ZERO), Word::ZERO);
            assert_eq!(state.storage_slot(&address, Word::ZERO).unwrap().current(), Word::ZERO);
        }
    }

    #[test]
    fn account_snapshot_deduplication_respects_nested_checkpoints_and_rollback() {
        let address = Address::with_last_byte(0x23);
        let mut state = State::new(CacheDB::default());
        let features = Version::base(SpecId::CANCUN).features;
        let parent = state.checkpoint();
        state.account(&address).unwrap().set_balance(Word::from(1));
        let child = state.checkpoint();
        assert_eq!(state.checkpoint(), child);
        state.account(&address).unwrap().set_balance(Word::from(2));
        state.account(&address).unwrap().set_balance(Word::from(3));
        assert_eq!(state.journal.len(), 2);
        let grandchild = state.checkpoint();
        state.account(&address).unwrap().set_balance(Word::from(4));
        state.account(&address).unwrap().set_balance(Word::from(5));
        assert_eq!(state.journal.len(), 3);

        state.rollback(grandchild, features);
        assert_eq!(state.account(&address).unwrap().balance(), Word::from(3));
        state.rollback(child.clone(), features);
        assert_eq!(state.account(&address).unwrap().balance(), Word::from(1));
        state.account(&address).unwrap().set_balance(Word::from(6));
        state.account(&address).unwrap().set_balance(Word::from(7));
        assert_eq!(state.journal.len(), 2);
        state.rollback(child, features);
        assert_eq!(state.account(&address).unwrap().balance(), Word::from(1));
        state.rollback(parent, features);
        assert!(!state.account(&address).unwrap().exists());
    }

    #[test]
    fn state_snapshots_preserve_account_deduplication_boundaries() {
        let address = Address::with_last_byte(0x24);
        for new_checkpoint in [false, true] {
            let mut state = State::new(CacheDB::default());
            let mut checkpoint = state.checkpoint();
            state.account(&address).unwrap().set_balance(Word::from(1));
            if new_checkpoint {
                checkpoint = state.checkpoint();
            }
            let mut restored = state.snapshot().into_state(CacheDB::default());
            restored.account(&address).unwrap().set_balance(Word::from(2));
            restored.account(&address).unwrap().set_balance(Word::from(3));
            assert_eq!(restored.journal.len(), 1 + usize::from(new_checkpoint));

            restored.rollback(checkpoint, Version::base(SpecId::CANCUN).features);
            let account = restored.account(&address).unwrap();
            assert_eq!(account.exists(), new_checkpoint);
            assert_eq!(account.balance(), Word::from(u64::from(new_checkpoint)));
        }
    }

    #[test]
    fn clearing_transaction_state_resets_account_deduplication_boundary() {
        let address = Address::with_last_byte(0x25);
        let mut state = State::new(CacheDB::default());
        state.account(&address).unwrap().set_nonce(1);
        state.checkpoint();
        state.clear_transaction_state();
        state.account(&address).unwrap().set_nonce(2);
        state.account(&address).unwrap().set_nonce(3);
        assert_eq!(state.journal.len(), 1);
    }

    #[test]
    fn deduplicated_account_overrides_preserve_the_earliest_snapshot() {
        let address = Address::with_last_byte(0x26);
        for initial_override in [false, true] {
            let mut state = State::new(CacheDB::default());
            let checkpoint = state.checkpoint();
            {
                let mut account = state.account(&address).unwrap();
                if initial_override {
                    account.override_balance(Word::from(100));
                    account.override_nonce(7);
                } else {
                    account.set_balance(Word::from(100));
                    account.set_nonce(7);
                }
            }
            state.account(&address).unwrap().override_balance(Word::from(200));
            state.account(&address).unwrap().override_nonce(8);
            assert_eq!(state.journal.len(), 1);
            {
                let account = state.account(&address).unwrap();
                assert_eq!((account.balance(), account.nonce()), (Word::from(200), 8));
            }

            state.rollback(checkpoint, Version::base(SpecId::CANCUN).features);
            let account = state.account(&address).unwrap();
            if initial_override {
                assert_eq!((account.balance(), account.nonce()), (Word::from(100), 7));
            } else {
                assert!(!account.exists());
            }
        }
    }
}
