//! Owned pending transaction state detached from the EVM.

use super::{Account, AccountInfo, StateChangeSink, StateChangeSource, StorageSlot, Tracked};
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
    /// Accounts selfdestructed by the transaction.
    pub(crate) selfdestructs: AddressSet,
}

impl PendingState {
    /// Returns whether the transaction loaded no accounts and no storage.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
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
        let account = self.accounts.entry(address).or_default();
        *account = Account {
            original,
            present: current,
            code_changed,
            storage: core::mem::take(&mut account.storage),
            ..Account::default()
        };
    }

    /// Inserts a storage slot's transaction-boundary original and current values.
    ///
    /// # Panics
    ///
    /// Panics if the owning account has not been inserted with [`Self::insert_account`].
    pub fn insert_storage(&mut self, address: Address, key: Word, original: Word, current: Word) {
        self.accounts
            .get_mut(&address)
            .expect("insert the account before its storage")
            .storage
            .slots
            .insert(
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
        self.accounts.values().any(|account| {
            account.is_changed()
                || account.storage.wiped
                || account.storage.changed_slots().next().is_some()
        })
    }
}

impl StateChangeSource for PendingState {
    /// Visits the transaction's loaded entries account by account, in an unspecified account
    /// order. Each account's bytecode and storage precede its metadata.
    ///
    /// The same code hash may be visited more than once when several accounts share bytecode; sinks
    /// key bytecode by hash, so repeated visits are idempotent.
    ///
    /// Changed accounts — including created or selfdestructed accounts whose info ended up
    /// unchanged — go through [`StateChangeSink::account`]; loaded-but-unchanged entries go
    /// through the read callbacks.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        for (&address, entry) in &self.accounts {
            entry.visit(address, self.selfdestructs.contains(&address), sink)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            state.accounts[&address].storage.slots[&key].value,
            Tracked::from_parts(Word::from(2), Word::from(3))
        );
    }
}
