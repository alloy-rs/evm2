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
    /// Loaded or changed storage slots.
    pub slots: U256Map<Tracked<Word>>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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
}
