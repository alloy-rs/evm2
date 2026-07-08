//! Owned pending transaction state detached from the EVM.

use super::{Account, AccountChange, State, StateChanges, StorageOverlay};
use alloy_primitives::map::{AddressMap, AddressSet, U256Map};

/// A transaction's finalized-but-uncommitted state, moved out of the EVM.
///
/// Produced by [`ExecutedTx::detach_pending`](crate::ExecutedTx::detach_pending), this is the
/// transaction overlay exactly as execution left it: every account and storage slot loaded during
/// the transaction, each carrying its transaction-boundary original value next to its present
/// value. Two consumers draw from it:
///
/// - [`Bal::apply_pending_state`](crate::evm::Bal::apply_pending_state) folds it into an EIP-7928
///   Block Access List, recording loaded-but-unchanged entries as reads and changed ones as
///   writes — the same fold the EVM applies on transaction commit when its builder is enabled.
/// - [`Self::build_state_changes`] materializes the owned [`StateChanges`] change-set that
///   persistence consumers (e.g. reth) apply to the database.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingState {
    /// Accounts loaded by the transaction: transaction-boundary original info, present info, and
    /// account-lifetime flags.
    pub(crate) accounts: AddressMap<Account>,
    /// Per-account storage overlays loaded by the transaction.
    pub(crate) storage: AddressMap<StorageOverlay>,
    /// Accounts selfdestructed by the transaction.
    pub(crate) selfdestructs: AddressSet,
}

impl PendingState {
    /// Returns whether the transaction loaded no accounts and no storage.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty()
    }

    /// Builds the transaction's owned [`StateChanges`] transition.
    ///
    /// This is the same change-set [`ExecutedTx::detach`](crate::ExecutedTx::detach) materializes,
    /// so consumers persisting state changes keep working from a pending state that was detached
    /// for BAL construction.
    pub fn build_state_changes(&self) -> StateChanges {
        self.into()
    }
}

impl From<&PendingState> for StateChanges {
    fn from(pending: &PendingState) -> Self {
        let mut changes = Self::default();

        for (&address, entry) in &pending.accounts {
            changes.accounts.insert(
                address,
                AccountChange {
                    original: entry.original.clone(),
                    current: entry.present.clone(),
                    storage: U256Map::default(),
                    wipe_storage: false,
                    // `just_created` is preserved across selfdestruct finalization, so it also
                    // covers accounts that were created and then destroyed in the same
                    // transaction.
                    created: entry.just_created,
                    selfdestructed: pending.selfdestructs.contains(&address),
                },
            );
            if let Some(account) = entry.present.as_ref()
                && let Some((code_hash, code)) = State::changed_code(entry.code_changed, account)
            {
                changes.code.entry(code_hash).or_insert_with(|| code.clone());
            }
        }

        // Fold per-account storage in. Storage-only accounts were materialized when the pending
        // state was taken from the EVM, so the entry already exists and holds unchanged info.
        for (&address, overlay) in &pending.storage {
            let entry = changes.accounts.entry(address).or_default();
            entry.wipe_storage = overlay.wiped;
            entry.storage = overlay.slots.iter().map(|(&key, slot)| (key, slot.value)).collect();
        }

        changes
    }
}
