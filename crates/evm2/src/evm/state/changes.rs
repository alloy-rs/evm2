//! Owned materialized state changes.

use super::{
    AccountChangeRef, AccountInfo, AccountInfoRef, StateChangeSink, StateChangeSource,
    StorageChange, Tracked,
};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::vec::Vec;
use alloy_primitives::{
    Address,
    map::{AddressMap, B256Map, U256Map},
};

/// Complete owned state transition produced by a transaction.
///
/// `StateChanges` is the public, materialized post-transaction state returned in
/// [`crate::TxResultWithState`] and by detached transaction APIs. It contains every account and
/// storage slot that was loaded during transaction execution, not just the ones that changed;
/// consumers applying changes should filter with [`AccountChange::is_changed`] and
/// [`Tracked::is_changed`].
///
/// Logs are execution output rather than database state and are exposed on
/// [`crate::TxResult`] and [`crate::TxResultWithState`].
///
/// Consumers should apply database changes in this order:
///
/// 1. write bytecode from [`Self::code`] for every code hash they do not already have;
/// 2. for each account with [`AccountChange::is_storage_wiped`], delete all storage for that
///    account;
/// 3. apply each changed storage slot: a zero [`Tracked::current`] means delete the slot, otherwise
///    write the slot value;
/// 4. apply changed accounts: `current = Some(..)` means upsert the account, `current = None` means
///    delete the account.
///
/// Persistence consumers should prefer consuming changes through
/// [`StateChangeSource::visit`] with a [`StateChangeSink`], or stream them without materializing
/// this type at all via [`crate::evm::ExecutedTx::commit_with`]; both yield only changed entries,
/// deduplicated bytecode, and a deterministic application order.
///
/// `evm2` does not write to the backing database. These changes describe what
/// happened; applying them is the responsibility of the caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateChanges {
    /// Accounts loaded or changed by the transaction, keyed by address.
    pub accounts: AddressMap<AccountChange>,
    /// Newly created or modified bytecode keyed by code hash.
    ///
    /// This deduplicates code shared by multiple changed accounts; each changed account also
    /// holds its bytecode in [`AccountInfo::code`].
    pub code: B256Map<Bytecode>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StateChanges {
    /// Returns whether this transition contains no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Returns whether this transition contains any account or storage change.
    ///
    /// Loaded-but-unchanged accounts and storage slots are ignored.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.accounts.values().any(|account| {
            account.is_changed()
                || account.wipe_storage
                || account.changed_storage().next().is_some()
        })
    }

    /// Returns the accounts whose info changed during the transaction.
    ///
    /// Loaded-but-unchanged accounts are skipped; accounts with only storage changes are
    /// included.
    #[inline]
    pub fn changed_accounts(&self) -> impl Iterator<Item = (&Address, &AccountChange)> {
        self.accounts.iter().filter(|(_, account)| {
            account.is_changed()
                || account.wipe_storage
                || account.changed_storage().next().is_some()
        })
    }
}

/// State of a single account across a transaction.
///
/// An entry is present for every account loaded during execution and holds the account's storage
/// slots, including loaded-but-unchanged slots.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountChange {
    /// Account info at the beginning of the transaction. `None` means the account did not exist.
    pub original: Option<AccountInfo>,
    /// Account info after transaction execution and EVM account-lifetime rules have been
    /// evaluated. `None` is an explicit account deletion.
    ///
    /// Accounts whose code changed during the transaction always hold the new bytecode in
    /// [`AccountInfo::code`]; otherwise `code` may be `None` and must be resolved through the
    /// backing database by code hash.
    pub current: Option<AccountInfo>,
    /// Storage slots loaded or changed during the transaction.
    ///
    /// Each slot's [`Tracked::original`] value is the slot value at the beginning of the
    /// transaction, after any storage wipe/re-incarnation semantics that occurred before the slot
    /// was loaded. `current = 0` for a changed slot means the consumer should delete the slot.
    pub storage: U256Map<Tracked<Word>>,
    /// If true, delete all pre-existing storage for this account before applying
    /// [`Self::storage`]. This is used for selfdestruct and contract re-incarnation semantics
    /// using an explicit storage wipe marker.
    pub(crate) wipe_storage: bool,
    /// Whether the account was created during the transaction.
    pub(crate) created: bool,
    /// Whether the account was selfdestructed during the transaction.
    pub(crate) selfdestructed: bool,
}

impl AccountChange {
    /// Marks the account's pre-existing storage for deletion.
    #[inline]
    pub const fn mark_storage_wiped(&mut self) {
        self.wipe_storage = true;
    }

    /// Returns whether consumers must delete all pre-existing storage for this account before
    /// applying [`Self::storage`].
    ///
    /// This is used for selfdestruct and contract re-incarnation semantics using an explicit
    /// storage wipe marker.
    #[inline]
    pub const fn is_storage_wiped(&self) -> bool {
        self.wipe_storage
    }

    /// Marks the account as created during the transaction.
    #[inline]
    pub const fn mark_created(&mut self) {
        self.created = true;
    }

    /// Returns whether the account was created during the transaction.
    #[inline]
    pub const fn is_created(&self) -> bool {
        self.created
    }

    /// Marks the account as selfdestructed during the transaction.
    #[inline]
    pub const fn mark_selfdestruct(&mut self) {
        self.selfdestructed = true;
    }

    /// Returns whether the account was selfdestructed during the transaction.
    #[inline]
    pub const fn is_selfdestructed(&self) -> bool {
        self.selfdestructed
    }

    /// Returns whether the account is empty after the transaction by the Spurious Dragon
    /// definition.
    ///
    /// Deleted accounts are empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.current.as_ref().is_none_or(AccountInfo::is_empty)
    }

    /// Returns whether the account's info changed during the transaction.
    ///
    /// This compares balance, nonce, code hash, and existence; it does not consider storage.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.original != self.current
    }

    /// Returns the changed storage slots of this account.
    #[inline]
    pub fn changed_storage(&self) -> impl Iterator<Item = (&Word, &Tracked<Word>)> {
        self.storage.iter().filter(|(_, slot)| {
            slot.is_changed() && (!self.wipe_storage || !slot.current.is_zero())
        })
    }
}

impl StateChangeSource for StateChanges {
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        let mut code_entries = self.code.iter().collect::<Vec<_>>();
        code_entries.sort_by_key(|(code_hash, _)| **code_hash);
        for (&code_hash, code) in code_entries {
            sink.bytecode(code_hash, code)?;
        }

        let mut account_entries = self.accounts.iter().collect::<Vec<_>>();
        account_entries.sort_by_key(|entry| *entry.0);

        for &(&address, change) in &account_entries {
            if change.wipe_storage {
                sink.storage_wipe(address)?;
            }

            let mut slots = change.changed_storage().collect::<Vec<_>>();
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

        for &(&address, change) in &account_entries {
            if !change.is_changed() {
                continue;
            }
            sink.account(AccountChangeRef {
                address,
                original: change.original.as_ref().map(AccountInfoRef::from_info),
                current: change.current.as_ref().map(AccountInfoRef::from_info),
            })?;
        }
        Ok(())
    }
}
