//! Public write-set emitted by a transaction.

use super::{AccountInfo, Tracked};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{collections::BTreeMap, vec::Vec};
use alloy_primitives::{Address, B256, Log};

/// Complete state transition and emitted logs produced by a transaction.
///
/// `StateChanges` is the public write-set returned in [`crate::TxResult`]. It
/// is intentionally explicit so embedding clients can update their own database
/// and compute post-state roots without reimplementing EVM account-lifetime
/// rules. It also carries the logs emitted by the transaction.
///
/// Consumers should apply database changes in this order:
///
/// 1. write bytecode from [`Self::code`] for every non-empty code hash they do not already have;
/// 2. for each [`StorageChangeSet`] whose [`StorageChangeSet::wipe`] flag is true, delete all
///    storage for that account;
/// 3. apply each storage slot change: a zero [`Tracked::current`] means delete the slot, otherwise
///    write the slot value;
/// 4. apply account changes: `current = Some(..)` means upsert the account, `current = None` means
///    delete the account.
///
/// `evm2` does not write to the backing database. These changes describe what
/// happened; applying them is the responsibility of the caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateChanges {
    /// Account changes keyed by address.
    ///
    /// [`Tracked::original`] is the account at the beginning of the transaction.
    /// [`Tracked::current`] is the account after transaction execution and EVM
    /// account-lifetime rules have been evaluated. `current = None` is an explicit account
    /// deletion.
    pub accounts: BTreeMap<Address, Tracked<Option<AccountInfo>>>,
    /// Persistent storage changes keyed by account address.
    ///
    /// Each slot change's [`Tracked::original`] value is the slot value at the beginning of the
    /// transaction, after any storage wipe/re-incarnation semantics that occurred before the slot
    /// was loaded. `current = 0` means the consumer should delete the slot.
    pub storage: BTreeMap<Address, StorageChangeSet>,
    /// Newly created or modified bytecode keyed by code hash.
    pub code: BTreeMap<B256, Bytecode>,
    /// Logs emitted by the transaction.
    pub logs: Vec<Log>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StateChanges {
    /// Returns whether this transition contains no changes or logs.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
            && self.storage.is_empty()
            && self.code.is_empty()
            && self.logs.is_empty()
    }
}

/// Storage transition for a single account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageChangeSet {
    /// If true, delete all pre-existing storage for this account before applying
    /// [`Self::slots`]. This is used for selfdestruct and contract
    /// re-incarnation semantics using an explicit storage wipe marker.
    pub wipe: bool,
    /// Changed storage slots keyed by slot.
    pub slots: BTreeMap<Word, Tracked<Word>>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        constants::EIP7708_BURN_TOPIC,
        evm::{CacheDB, state::State},
    };
    use alloy_primitives::Bytes;

    #[test]
    fn state_changes_take_logs_from_transaction_state() {
        use alloy_primitives::{Bytes, LogData};

        let mut state = State::new(CacheDB::default());
        let log = Log {
            address: Address::from([0x66; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x03])),
        };

        state.log(log.clone());
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();
        assert_eq!(changes.logs.as_slice(), core::slice::from_ref(&log));
        assert!(state.logs().is_empty());

        state.commit_transaction_overlay();
        state.clear_transaction_state();
        assert!(state.logs().is_empty());
    }

    #[test]
    fn spurious_dragon_deletes_touched_empty_existing_account() {
        let address = Address::from([0x44; 20]);
        let empty = AccountInfo { code: None, ..AccountInfo::default() };
        let mut database = CacheDB::default();
        database.insert_account_info(&address, empty.clone());
        let mut state = State::new(database);

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        let change = changes.accounts.get(&address).expect("touched empty account is deleted");
        assert_eq!(change.original, Some(empty));
        assert_eq!(change.current, None);
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
    }

    #[test]
    fn homestead_preserves_touched_empty_existing_account() {
        let address = Address::from([0x45; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        let mut state = State::new(database);

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::HOMESTEAD));
        let changes = state.build_state_changes();

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn homestead_materializes_touched_empty_new_account() {
        let address = Address::from([0x46; 20]);
        let mut state = State::new(CacheDB::default());

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::HOMESTEAD));
        let changes = state.build_state_changes();

        let change =
            changes.accounts.get(&address).expect("pre-spurious empty touch creates account");
        assert_eq!(change.original, None);
        let current = change.current.as_ref().expect("created empty account");
        assert!(current.is_empty());
    }

    #[test]
    fn spurious_dragon_ignores_touched_empty_new_account() {
        let address = Address::from([0x47; 20]);
        let mut state = State::new(CacheDB::default());

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn finalization_preserves_touched_set_capacity() {
        let mut state = State::new(CacheDB::default());

        for i in 0..32 {
            state.touch(&Address::from([i; 20]));
            state.mark_destructed(&Address::from([i + 32; 20]));
        }

        let touched_capacity = state.touched.capacity();
        let selfdestructs_capacity = state.selfdestructs.capacity();

        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));

        assert!(state.touched.is_empty());
        assert!(state.selfdestructs.is_empty());
        assert_eq!(state.touched.capacity(), touched_capacity);
        assert_eq!(state.selfdestructs.capacity(), selfdestructs_capacity);
    }

    #[test]
    fn selfdestruct_deletes_account_and_wipes_storage() {
        let address = Address::from([0x48; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&address, &Word::from(1), &Word::from(2));
        let mut state = State::new(database);

        state.mark_destructed(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        let change = changes.accounts.get(&address).expect("selfdestruct deletes account");
        assert!(change.original.is_some());
        assert_eq!(change.current, None);
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
    }

    #[test]
    fn eip7708_delayed_burn_logs_selfdestructs_sorted() {
        let high = Address::from([0x22; 20]);
        let low = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&high, AccountInfo::default().with_balance(Word::from(2)));
        database.insert_account_info(&low, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        state.mark_destructed(&high);
        state.mark_destructed(&low);
        let mut inspected = Vec::new();
        state
            .finalize_transaction(Version::base(SpecId::AMSTERDAM), |log| {
                inspected.push(log.clone())
            })
            .unwrap();

        let changes = state.build_state_changes();
        assert_eq!(inspected, changes.logs);
        assert_eq!(changes.logs.len(), 2);
        assert_eq!(
            changes.logs[0].topics(),
            &[EIP7708_BURN_TOPIC, B256::left_padding_from(low.as_slice())]
        );
        assert_eq!(
            changes.logs[0].data.data,
            Bytes::copy_from_slice(&Word::from(1).to_be_bytes::<32>())
        );
        assert_eq!(
            changes.logs[1].topics(),
            &[EIP7708_BURN_TOPIC, B256::left_padding_from(high.as_slice())]
        );
        assert_eq!(
            changes.logs[1].data.data,
            Bytes::copy_from_slice(&Word::from(2).to_be_bytes::<32>())
        );
    }
}
