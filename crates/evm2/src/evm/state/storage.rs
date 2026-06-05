//! Transaction-scoped persistent storage overlay.

use super::Tracked;
use crate::interpreter::Word;
use alloy_primitives::map::U256Map;

/// Persistent storage overlay for one account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageOverlay {
    /// Whether consumers must delete all pre-existing storage for the account
    /// before applying individual slot changes.
    pub wiped: bool,
    /// Loaded, changed, or warmed storage slots.
    pub slots: U256Map<StorageSlot>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Persistent storage slot metadata cached by [`super::State`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageSlot {
    /// Loaded storage value, if the slot value has been read or written.
    pub value: Option<Tracked<Word>>,
    /// Whether this storage slot is warm in the current transaction.
    pub warm: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StorageSlot {
    #[inline]
    pub(super) const fn is_empty(&self) -> bool {
        self.value.is_none() && !self.warm
    }

    #[inline]
    pub(super) const fn mark_warm(&mut self) -> bool {
        let was_cold = !self.warm;
        self.warm = true;
        was_cold
    }
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
    use alloy_primitives::Address;

    #[test]
    fn storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &Word::from(1), &Word::from(10));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.set_storage(&address, &Word::from(1), &Word::from(20)).unwrap();
        state.set_storage(&address, &Word::from(1), &Word::from(30)).unwrap();

        assert_eq!(state.storage(&address, &Word::from(1)).unwrap(), Word::from(30));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.storage(&address, &Word::from(1)).unwrap(), Word::from(10));
    }

    #[test]
    fn transient_storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x22; 20]);
        let mut state = State::new(CacheDB::default());

        state.set_transient_storage(&address, &Word::from(1), &Word::from(10));
        let checkpoint = state.checkpoint();
        state.set_transient_storage(&address, &Word::from(1), &Word::from(20));

        assert_eq!(state.transient_storage(&address, &Word::from(1)), Word::from(20));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.transient_storage(&address, &Word::from(1)), Word::from(10));
    }

    #[test]
    fn storage_wipe_preserves_warm_slots_in_merged_storage_map() {
        let account = Address::with_last_byte(0x19);
        let warm_key = Word::from(1);
        let cold_key = Word::from(2);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&account, &warm_key, &Word::from(3));
        database.insert_account_storage(&account, &cold_key, &Word::from(4));
        let mut state = State::new(database);

        assert!(state.warm_storage_non_revertible(&account, &warm_key));
        state.set_storage(&account, &cold_key, &Word::from(5)).unwrap();

        state.wipe_storage(&account);
        assert!(state.is_storage_warm(&account, &warm_key));
        assert!(!state.is_storage_warm(&account, &cold_key));
        assert_eq!(state.storage_ref(&account, &warm_key), Some(Word::ZERO));
        assert_eq!(state.storage_ref(&account, &cold_key), Some(Word::ZERO));

        let changes = state.build_state_changes();
        let storage = changes.storage.get(&account).expect("wipe must be emitted");
        assert!(storage.wipe);
        assert!(storage.slots.is_empty());
    }
}
