//! Owned materialized state changes.

use super::{
    AccountChangeRef, AccountInfo, AccountInfoRef, StateChangeSink, StateChangeSource,
    StorageChange, Tracked,
};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::vec::Vec;
use alloy_primitives::map::{AddressMap, B256Map, U256Map};

/// Complete owned state transition produced by a transaction.
///
/// `StateChanges` is the public, materialized write-set returned in
/// [`crate::TxResultWithState`] and by detached transaction APIs. It is intentionally
/// explicit so embedding clients can update their own database and compute
/// post-state roots without reimplementing EVM account-lifetime rules.
///
/// Logs are execution output rather than database state and are exposed on
/// [`crate::TxResult`] and [`crate::TxResultWithState`].
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
    pub accounts: AddressMap<Tracked<Option<AccountInfo>>>,
    /// Persistent storage changes keyed by account address.
    ///
    /// Each slot change's [`Tracked::original`] value is the slot value at the beginning of the
    /// transaction, after any storage wipe/re-incarnation semantics that occurred before the slot
    /// was loaded. `current = 0` means the consumer should delete the slot.
    pub storage: AddressMap<StorageChangeSet>,
    /// Newly created or modified bytecode keyed by code hash.
    pub code: B256Map<Bytecode>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StateChanges {
    /// Returns whether this transition contains no changes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty() && self.code.is_empty()
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
    pub slots: U256Map<Tracked<Word>>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StateChangeSource for StateChanges {
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        let mut code_entries = self.code.iter().collect::<Vec<_>>();
        code_entries.sort_by_key(|(code_hash, _)| **code_hash);
        for (&code_hash, code) in code_entries {
            sink.bytecode(code_hash, code)?;
        }

        let mut storage_entries = self.storage.iter().collect::<Vec<_>>();
        storage_entries.sort_by_key(|entry| *entry.0);
        for (&address, storage) in storage_entries {
            if storage.wipe {
                sink.storage_wipe(address)?;
            }

            let mut slots = storage.slots.iter().collect::<Vec<_>>();
            slots.sort_by_key(|entry| *entry.0);
            for (&key, slot) in slots {
                sink.storage(StorageChange {
                    address,
                    key,
                    original: slot.original,
                    current: slot.current,
                })?;
            }
        }

        let mut account_entries = self.accounts.iter().collect::<Vec<_>>();
        account_entries.sort_by_key(|entry| *entry.0);
        for (&address, change) in account_entries {
            sink.account(AccountChangeRef {
                address,
                original: change.original.as_ref().map(AccountInfoRef::from_info),
                current: change.current.as_ref().map(AccountInfoRef::from_info),
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        constants::EIP7708_BURN_TOPIC,
        evm::{CacheDB, state::State},
    };
    use alloy_primitives::{Address, B256, Bytes, Log};

    #[test]
    fn build_state_changes_leaves_logs_on_transaction_state() {
        use alloy_primitives::LogData;

        let mut state = State::new(CacheDB::default());
        let log = Log {
            address: Address::from([0x66; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x03])),
        };

        state.log(log.clone());
        state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();
        assert!(changes.is_empty());
        assert_eq!(state.logs(), core::slice::from_ref(&log));

        state.commit_transaction();
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

        state.account_entry(&address, false).unwrap().touch();
        state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
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

        state.account_entry(&address, false).unwrap().touch();
        state.finalize_transaction_(Version::base(SpecId::HOMESTEAD));
        let changes = state.build_state_changes();

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn homestead_materializes_touched_empty_new_account() {
        let address = Address::from([0x46; 20]);
        let mut state = State::new(CacheDB::default());

        state.account_entry(&address, false).unwrap().touch();
        state.finalize_transaction_(Version::base(SpecId::HOMESTEAD));
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

        state.account_entry(&address, false).unwrap().touch();
        state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn finalization_clears_touched_account_entry_flags() {
        let mut state = State::new(CacheDB::default());

        for i in 0..32 {
            state.account_entry(&Address::from([i; 20]), false).unwrap().touch();
            state.account_entry(&Address::from([i + 32; 20]), false).unwrap().mark_destructed();
        }

        let selfdestructs_capacity = state.selfdestructs.capacity();

        state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));

        assert!(state.accounts.values().all(|entry| !entry.is_touched));
        assert!(state.selfdestructs.is_empty());
        assert_eq!(state.selfdestructs.capacity(), selfdestructs_capacity);
    }

    #[test]
    fn selfdestruct_deletes_account_and_wipes_storage() {
        let address = Address::from([0x48; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&address, &Word::from(1), &Word::from(2));
        let mut state = State::new(database);

        state.account_entry(&address, false).unwrap().mark_destructed();
        state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
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

        state.account_entry(&high, false).unwrap().mark_destructed();
        state.account_entry(&low, false).unwrap().mark_destructed();
        let mut inspected = Vec::new();
        state
            .finalize_transaction(Version::base(SpecId::AMSTERDAM), |log| {
                inspected.push(log.clone())
            })
            .unwrap();

        // Burn logs are execution output and stay on the transaction state, not in StateChanges.
        let logs: Vec<Log> = state.logs().to_vec();
        let _changes = state.build_state_changes();
        assert_eq!(inspected, logs);
        assert_eq!(logs.len(), 2);
        assert_eq!(
            logs[0].topics(),
            &[EIP7708_BURN_TOPIC, B256::left_padding_from(low.as_slice())]
        );
        assert_eq!(logs[0].data.data, Bytes::copy_from_slice(&Word::from(1).to_be_bytes::<32>()));
        assert_eq!(
            logs[1].topics(),
            &[EIP7708_BURN_TOPIC, B256::left_padding_from(high.as_slice())]
        );
        assert_eq!(logs[1].data.data, Bytes::copy_from_slice(&Word::from(2).to_be_bytes::<32>()));
    }
}
