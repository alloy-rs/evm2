//! Owned pending transaction state detached from the EVM.

use super::{
    Account, AccountChangeRef, AccountInfo, StateChangeSink, StateChangeSource, StorageChange,
    StorageOverlay, StorageSlot, Tracked,
};
use crate::interpreter::Word;
use alloy_primitives::{
    Address,
    map::{AddressMap, AddressSet},
};

/// A transaction's finalized-but-uncommitted state, moved out of the EVM.
///
/// Produced by [`ExecutedTx::detach`](crate::ExecutedTx::detach), this is the transaction overlay
/// exactly as execution left it: every account and storage slot loaded during the transaction,
/// each carrying its transaction-boundary original value next to its present value. Two consumers
/// draw from it:
///
/// - [`Bal::commit`](crate::evm::Bal::commit) folds it into an EIP-7928 Block Access List,
///   recording loaded-but-unchanged entries as reads and changed ones as writes — the same fold the
///   EVM applies on transaction commit when its builder is enabled.
/// - [`StateChangeSource::visit`] streams it to a [`StateChangeSink`] in deterministic application
///   order: changed entries through the change callbacks (how persistence consumers, e.g. reth,
///   apply the transaction to the database) and loaded-but-unchanged entries through the read
///   callbacks.
///
/// A detached pending state can also be reattached to an EVM with
/// [`State::set_pending_state`](super::State::set_pending_state).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingState {
    /// Accounts loaded by the transaction: transaction-boundary original info, present info, and
    /// account-lifetime flags.
    pub(crate) accounts: AddressMap<Account>,
    /// Per-account storage overlays loaded by the transaction.
    ///
    /// Accounts whose storage was loaded are normally present in [`Self::accounts`] as well, since
    /// executing an account loads it.
    pub(crate) storage: AddressMap<StorageOverlay>,
    /// Accounts selfdestructed by the transaction.
    pub(crate) selfdestructs: AddressSet,
}

impl PendingState {
    /// Owns a borrowed transaction change stream, including account lifecycle markers.
    ///
    /// Native execution already produces this representation; this constructor is for sources
    /// such as system updates that only expose callbacks.
    pub fn from_source(source: &impl StateChangeSource) -> Self {
        #[derive(Default)]
        struct Builder {
            pending: PendingState,
            code: alloy_primitives::map::B256Map<crate::bytecode::Bytecode>,
        }
        impl StateChangeSink for Builder {
            type Error = core::convert::Infallible;
            fn bytecode(
                &mut self,
                hash: alloy_primitives::B256,
                code: &crate::bytecode::Bytecode,
            ) -> Result<(), Self::Error> {
                self.code.entry(hash).or_insert_with(|| code.clone());
                Ok(())
            }
            fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
                let storage = self.pending.storage.entry(address).or_default();
                storage.wiped = true;
                storage.slots.clear();
                Ok(())
            }
            fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
                self.pending.insert_storage(
                    change.address,
                    change.key,
                    change.original,
                    change.current,
                );
                Ok(())
            }
            fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
                let mut current = change.current.cloned();
                if let Some(info) = &mut current
                    && info.code.is_none()
                {
                    info.code = self.code.get(&info.code_hash).cloned();
                }
                self.pending.insert_account(change.address, change.original.cloned(), current);
                let account = self.pending.accounts.get_mut(&change.address).unwrap();
                account.just_created = change.created;
                if change.selfdestructed {
                    self.pending.selfdestructs.insert(change.address);
                }
                Ok(())
            }
            fn account_read(
                &mut self,
                address: Address,
                info: Option<&AccountInfo>,
            ) -> Result<(), Self::Error> {
                self.pending.accounts.entry(address).or_insert_with(|| Account {
                    original: info.cloned(),
                    present: info.cloned(),
                    ..Default::default()
                });
                Ok(())
            }
            fn storage_read(
                &mut self,
                address: Address,
                key: Word,
                value: Word,
            ) -> Result<(), Self::Error> {
                self.pending.insert_storage(address, key, value, value);
                Ok(())
            }
        }
        let mut builder = Builder::default();
        let Ok(()) = source.visit(&mut builder);
        builder.pending
    }

    /// Borrows changed accounts together with their transaction storage overlays.
    ///
    /// Includes accounts whose metadata stayed unchanged but whose storage changed or was wiped.
    /// Consumers can apply [`StorageOverlay::changed_slots`] directly without regrouping the
    /// flat change stream. Loaded-but-unchanged accounts and slots are omitted.
    pub fn changed_accounts(
        &self,
    ) -> impl Iterator<Item = (AccountChangeRef<'_>, Option<&StorageOverlay>)> {
        self.accounts.iter().filter_map(|(&address, entry)| {
            let storage = self.storage.get(&address);
            let selfdestructed = self.selfdestructs.contains(&address);
            (entry.is_changed()
                || entry.is_created()
                || selfdestructed
                || storage.is_some_and(|s| s.wiped || s.changed_slots().next().is_some()))
            .then_some((
                AccountChangeRef {
                    address,
                    original: entry.original.as_ref(),
                    current: entry.present.as_ref(),
                    created: entry.is_created(),
                    selfdestructed,
                },
                storage,
            ))
        })
    }

    /// Borrows bytecode changed by the transaction, keyed by code hash.
    pub fn changed_bytecodes(
        &self,
    ) -> impl Iterator<Item = (alloy_primitives::B256, &crate::bytecode::Bytecode)> {
        self.accounts.values().filter_map(Account::changed_code)
    }

    /// Returns whether the transaction loaded no accounts and no storage.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty()
    }

    /// Returns the current account information when the account is present in pending state.
    #[inline]
    pub fn account_info(&self, address: &Address) -> Option<&AccountInfo> {
        self.accounts.get(address).and_then(|account| account.present.as_ref())
    }

    /// Inserts an account's transaction-boundary original and current values.
    pub fn insert_account(
        &mut self,
        address: Address,
        original: Option<AccountInfo>,
        current: Option<AccountInfo>,
    ) {
        let code_changed = original.as_ref().map(|account| account.code_hash)
            != current.as_ref().map(|account| account.code_hash);
        self.accounts.insert(
            address,
            Account { original, present: current, code_changed, ..Account::default() },
        );
    }

    /// Inserts a storage slot's transaction-boundary original and current values.
    pub fn insert_storage(&mut self, address: Address, key: Word, original: Word, current: Word) {
        self.storage.entry(address).or_default().slots.insert(
            key,
            StorageSlot {
                value: Tracked::from_parts(original, current),
                is_warm: false,
                _non_exhaustive: (),
            },
        );
    }

    /// Returns whether the transaction contains any account or storage change.
    ///
    /// Loaded-but-unchanged accounts and storage slots are ignored.
    #[cfg(test)]
    pub(crate) fn is_changed(&self) -> bool {
        self.accounts.values().any(Account::is_changed)
            || self
                .storage
                .values()
                .any(|overlay| overlay.wiped || overlay.changed_slots().next().is_some())
    }
}

impl StateChangeSource for PendingState {
    /// Visits the transaction's loaded entries in an unspecified order: bytecode, then per-account
    /// storage wipes, changed slots, and slot reads, then accounts.
    ///
    /// The same code hash may be visited more than once when several accounts share bytecode; sinks
    /// key bytecode by hash, so repeated visits are idempotent.
    ///
    /// Changed accounts — including created or selfdestructed accounts whose info ended up
    /// unchanged — go through [`StateChangeSink::account`]; loaded-but-unchanged entries go
    /// through the read callbacks.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        for (code_hash, code) in self.accounts.values().filter_map(Account::changed_code) {
            sink.bytecode(code_hash, code)?;
        }

        for (&address, overlay) in &self.storage {
            if overlay.wiped {
                sink.storage_wipe(address)?;
            }
            for (&key, slot) in &overlay.slots {
                let value = &slot.value;
                if slot.is_changed(overlay.wiped) {
                    sink.storage(StorageChange {
                        address,
                        key,
                        original: value.original,
                        current: value.current,
                    })?;
                } else {
                    sink.storage_read(address, key, value.current)?;
                }
            }
        }

        for (&address, entry) in &self.accounts {
            let selfdestructed = self.selfdestructs.contains(&address);
            if entry.is_changed() || entry.is_created() || selfdestructed {
                sink.account(AccountChangeRef {
                    address,
                    original: entry.original.as_ref(),
                    current: entry.present.as_ref(),
                    created: entry.is_created(),
                    selfdestructed,
                })?;
            } else {
                sink.account_read(address, entry.present.as_ref())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_changes_reinsert_unchanged_nonzero_storage_after_wipe() {
        let address = Address::with_last_byte(1);
        let info = AccountInfo { nonce: 1, ..Default::default() };
        let mut pending = PendingState::default();
        pending.insert_account(address, Some(info.clone()), Some(info));
        pending.insert_storage(address, Word::from(1), Word::from(7), Word::from(7));
        pending.insert_storage(address, Word::from(2), Word::from(8), Word::ZERO);
        assert!(pending.changed_accounts().next().is_some());
        pending.storage.get_mut(&address).unwrap().wiped = true;
        let changes = pending.changed_accounts().collect::<Vec<_>>();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0.address, address);
        let storage = changes[0].1.unwrap();
        assert!(storage.wiped);
        let slots = storage.changed_slots().collect::<Vec<_>>();
        assert_eq!(slots.len(), 1);
        assert_eq!(*slots[0].0, Word::from(1));
        assert_eq!(slots[0].1.current, Word::from(7));
        let rebuilt = PendingState::from_source(&pending);
        let rebuilt_changes = rebuilt.changed_accounts().collect::<Vec<_>>();
        assert_eq!(rebuilt_changes.len(), 1);
        assert!(rebuilt_changes[0].1.unwrap().wiped);
        assert_eq!(rebuilt_changes[0].1.unwrap().changed_slots().count(), 1);
        pending.accounts.get_mut(&address).unwrap().just_created = true;
        pending.selfdestructs.insert(address);
        let rebuilt = PendingState::from_source(&pending);
        let change = rebuilt.changed_accounts().next().unwrap().0;
        assert!(change.created);
        assert!(change.selfdestructed);
    }

    #[test]
    fn inserts_account_and_storage() {
        let address = Address::with_last_byte(0xaa);
        let key = Word::from(1);
        let original = AccountInfo { nonce: 1, ..AccountInfo::default() };
        let current = AccountInfo { nonce: 2, ..AccountInfo::default() };
        let mut state = PendingState::default();

        state.insert_account(address, Some(original.clone()), Some(current.clone()));
        state.insert_storage(address, key, Word::from(2), Word::from(3));

        assert_eq!(state.account_info(&address), Some(&current));
        assert_eq!(state.accounts[&address].original, Some(original));
        assert_eq!(state.accounts[&address].present, Some(current));
        assert_eq!(
            state.storage[&address].slots[&key].value,
            Tracked::from_parts(Word::from(2), Word::from(3))
        );
    }
}
