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
/// [`crate::TxResult`] and by detached transaction APIs. It is intentionally
/// explicit so embedding clients can update their own database and compute
/// post-state roots without reimplementing EVM account-lifetime rules.
///
/// Logs are execution output rather than database state and are exposed on
/// [`crate::TxOutcome`] and [`crate::TxResult`].
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
