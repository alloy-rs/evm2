//! Owned pending transaction state detached from the EVM.

use super::{
    Account, AccountChangeRef, AccountInfoRef, StateChangeSink, StateChangeSource, StorageChange,
    StorageOverlay,
};
use alloc::vec::Vec;
use alloy_primitives::map::{AddressMap, AddressSet};

/// A transaction's finalized-but-uncommitted state, moved out of the EVM.
///
/// Produced by [`ExecutedTx::detach`](crate::ExecutedTx::detach), this is the transaction overlay
/// exactly as execution left it: every account and storage slot loaded during the transaction,
/// each carrying its transaction-boundary original value next to its present value. Two consumers
/// draw from it:
///
/// - [`Bal::apply_pending_state`](crate::evm::Bal::apply_pending_state) folds it into an EIP-7928
///   Block Access List, recording loaded-but-unchanged entries as reads and changed ones as writes
///   — the same fold the EVM applies on transaction commit when its builder is enabled.
/// - [`StateChangeSource::visit`] streams the changed entries to a [`StateChangeSink`] in
///   deterministic application order, which is how persistence consumers (e.g. reth) apply the
///   transaction to the database.
///
/// A detached pending state can also be reattached to an EVM with
/// [`State::set_pending_state`](super::State::set_pending_state).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingState {
    /// Accounts loaded by the transaction: transaction-boundary original info, present info, and
    /// account-lifetime flags.
    pub accounts: AddressMap<Account>,
    /// Per-account storage overlays loaded by the transaction.
    ///
    /// Accounts whose storage was loaded are normally present in [`Self::accounts`] as well, since
    /// executing an account loads it.
    pub storage: AddressMap<StorageOverlay>,
    /// Accounts selfdestructed by the transaction.
    pub selfdestructs: AddressSet,
}

impl PendingState {
    /// Returns whether the transaction loaded no accounts and no storage.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty()
    }

    /// Returns whether the transaction contains any account or storage change.
    ///
    /// Loaded-but-unchanged accounts and storage slots are ignored.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.accounts.values().any(Account::is_changed)
            || self
                .storage
                .values()
                .any(|overlay| overlay.wiped || overlay.changed_slots().next().is_some())
    }
}

impl StateChangeSource for PendingState {
    /// Visits the transaction's changed entries in deterministic application order: deduplicated
    /// bytecode sorted by code hash, then per-account storage wipes and changed slots sorted by
    /// address and key, then changed accounts sorted by address.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        let mut code_entries =
            self.accounts.values().filter_map(Account::changed_code).collect::<Vec<_>>();
        code_entries.sort_by_key(|(code_hash, _)| *code_hash);
        code_entries.dedup_by_key(|(code_hash, _)| *code_hash);
        for (code_hash, code) in code_entries {
            sink.bytecode(code_hash, code)?;
        }

        let mut storage_entries = self.storage.iter().collect::<Vec<_>>();
        storage_entries.sort_by_key(|entry| *entry.0);
        for (&address, overlay) in storage_entries {
            if overlay.wiped {
                sink.storage_wipe(address)?;
            }
            let mut slots = overlay.changed_slots().collect::<Vec<_>>();
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
        for (&address, entry) in account_entries {
            if !entry.is_changed() {
                continue;
            }
            sink.account(AccountChangeRef {
                address,
                original: entry.original.as_ref().map(AccountInfoRef::from_info),
                current: entry.present.as_ref().map(AccountInfoRef::from_info),
            })?;
        }
        Ok(())
    }
}
