//! Materialized per-transaction state collected from a pending-state stream.

use alloy_primitives::{
    Address,
    map::{AddressMap, U256Map},
};
use core::convert::Infallible;
use evm2::{
    evm::{
        AccountChangeRef, AccountInfo, PendingState, StateChangeSink, StateChangeSource,
        StorageChange, Tracked,
    },
    interpreter::Word,
};

/// A transaction's loaded accounts and storage, materialized from the [`PendingState`] change
/// stream for random access by trace builders.
///
/// Contains every account and storage slot the transaction loaded: changed entries arrive through
/// the [`StateChangeSink`] change callbacks, loaded-but-unchanged ones through its read callbacks.
#[derive(Debug, Default)]
pub(crate) struct TxState {
    /// Accounts loaded by the transaction, keyed by address.
    pub(crate) accounts: AddressMap<TxAccount>,
}

impl TxState {
    /// Collects a transaction's detached pending state into a materialized view.
    pub(crate) fn from_pending(pending: &PendingState) -> Self {
        let mut state = Self::default();
        let Ok(()) = pending.visit(&mut state);
        state
    }
}

/// One account loaded by the transaction, with its loaded storage slots.
#[derive(Debug, Default)]
pub(crate) struct TxAccount {
    /// Account info at the start of the transaction. `None` means the account did not exist.
    pub(crate) original: Option<AccountInfo>,
    /// Account info after the transaction. `None` is an explicit deletion.
    pub(crate) current: Option<AccountInfo>,
    /// Whether the account was created during the transaction.
    pub(crate) created: bool,
    /// Whether the account was selfdestructed during the transaction.
    pub(crate) selfdestructed: bool,
    /// Storage slots loaded during the transaction, changed and unchanged.
    pub(crate) storage: U256Map<Tracked<Word>>,
}

impl TxAccount {
    /// Returns the storage slots the transaction changed.
    pub(crate) fn changed_storage(&self) -> impl Iterator<Item = (&Word, &Tracked<Word>)> {
        self.storage.iter().filter(|(_, slot)| slot.is_changed())
    }
}

fn clone_account_info(target: &mut Option<AccountInfo>, source: Option<&AccountInfo>) {
    match (target, source) {
        (Some(target), Some(source)) => target.clone_from(source),
        (target, Some(source)) => *target = Some(source.clone()),
        (target, None) => *target = None,
    }
}

impl StateChangeSink for TxState {
    type Error = Infallible;

    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        let entry = self.accounts.entry(change.address).or_default();
        clone_account_info(&mut entry.original, change.original);
        clone_account_info(&mut entry.current, change.current);
        entry.created = change.created;
        entry.selfdestructed = change.selfdestructed;
        Ok(())
    }

    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.accounts
            .entry(change.address)
            .or_default()
            .storage
            .insert(change.key, Tracked::from_parts(change.original, change.current));
        Ok(())
    }

    fn account_read(
        &mut self,
        address: Address,
        info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        let entry = self.accounts.entry(address).or_default();
        clone_account_info(&mut entry.original, info);
        entry.current.clone_from(&entry.original);
        Ok(())
    }

    fn storage_read(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
    ) -> Result<(), Self::Error> {
        self.accounts.entry(address).or_default().storage.insert(key, Tracked::new(value));
        Ok(())
    }
}
