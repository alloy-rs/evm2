//! Borrowed state-change streaming traits and adapters.

use super::{AccountInfo, StorageOverlay};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{Address, B256};
use auto_impl::auto_impl;
use core::convert::Infallible;

/// Borrowed account change passed to [`StateChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountChangeRef<'a> {
    /// Account address.
    pub address: Address,
    /// Account at the start of the source's aggregation boundary.
    pub original: Option<&'a AccountInfo>,
    /// Account after the change. `None` is an explicit deletion.
    pub current: Option<&'a AccountInfo>,
    /// Whether the account was created during the transaction.
    ///
    /// Only transaction-level sources report this; block-level aggregation loses per-transaction
    /// lifecycle flags and reports `false`.
    pub created: bool,
    /// Whether the account was selfdestructed during the transaction.
    ///
    /// Only transaction-level sources report this; block-level aggregation loses per-transaction
    /// lifecycle flags and reports `false`.
    pub selfdestructed: bool,
}

/// One account's changes passed to [`StateChangeSink::account_changes`]: its storage overlay and
/// its metadata, delivered together.
#[derive(Clone, Copy, Debug)]
pub struct AccountChanges<'a> {
    /// Account address.
    pub address: Address,
    /// Account at the start of the source's aggregation boundary.
    pub original: Option<&'a AccountInfo>,
    /// Account after the changes. `None` is an explicit deletion.
    pub current: Option<&'a AccountInfo>,
    /// Whether the account metadata changed, including creation or selfdestruct. `false` means
    /// the account was only loaded.
    pub changed: bool,
    /// Whether the account was created during the transaction.
    pub created: bool,
    /// Whether the account was selfdestructed during the transaction.
    pub selfdestructed: bool,
    /// The account's loaded storage slots and wipe marker.
    pub storage: &'a StorageOverlay,
}

impl<'a> AccountChanges<'a> {
    /// Returns the account metadata change, or `None` when the account was only loaded.
    #[inline]
    pub const fn change(&self) -> Option<AccountChangeRef<'a>> {
        if !self.changed {
            return None;
        }
        Some(AccountChangeRef {
            address: self.address,
            original: self.original,
            current: self.current,
            created: self.created,
            selfdestructed: self.selfdestructed,
        })
    }

    /// Returns the changed storage slots. A wiped overlay reports every nonzero current value.
    #[inline]
    pub fn storage_changes(&self) -> impl Iterator<Item = StorageChange> + '_ {
        let address = self.address;
        self.storage.changed_slots().map(move |(&key, value)| StorageChange {
            address,
            key,
            original: value.original,
            current: value.current,
        })
    }
}

/// Storage slot change passed to [`StateChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageChange {
    /// Account address.
    pub address: Address,
    /// Storage slot key.
    pub key: Word,
    /// Slot value at the start of the source's aggregation boundary.
    pub original: Word,
    /// Slot value after the change.
    pub current: Word,
}

/// Consumer of borrowed transaction or block state changes.
#[auto_impl(&mut, Box)]
pub trait StateChangeSink {
    /// Error returned by this sink.
    type Error;

    /// Observes bytecode keyed by code hash.
    #[inline]
    fn bytecode(&mut self, _code_hash: B256, _code: &Bytecode) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes one account's storage and metadata together.
    ///
    /// Transaction-level sources call this once per loaded account, after the account's bytecode.
    /// The default replays the per-entry callbacks: the storage wipe, then changed slots through
    /// [`Self::storage`] and unchanged slots through [`Self::storage_read`], then the metadata
    /// through [`Self::account`] or [`Self::account_read`].
    #[inline]
    fn account_changes(&mut self, changes: AccountChanges<'_>) -> Result<(), Self::Error> {
        let address = changes.address;
        let storage = changes.storage;
        if storage.wiped {
            self.storage_wipe(address)?;
        }
        for (&key, slot) in &storage.slots {
            let value = &slot.value;
            if slot.is_changed(storage.wiped) {
                self.storage(StorageChange {
                    address,
                    key,
                    original: value.original,
                    current: value.current,
                })?;
            } else {
                self.storage_read(address, key, value.current)?;
            }
        }
        match changes.change() {
            Some(change) => self.account(change),
            None => self.account_read(address, changes.current),
        }
    }

    /// Observes an account change.
    #[inline]
    fn account(&mut self, _change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage wipe marker for an account.
    ///
    /// Sources emit this before any storage slot changes for the same account so sinks can apply
    /// the wipe once, then apply subsequent slot writes.
    #[inline]
    fn storage_wipe(&mut self, _address: Address) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage slot change.
    #[inline]
    fn storage(&mut self, _change: StorageChange) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes an account the transaction loaded but left unchanged. `None` means the account
    /// was loaded as non-existent.
    ///
    /// Only transaction-level sources report reads; sinks that persist changes can ignore them.
    #[inline]
    fn account_read(
        &mut self,
        _address: Address,
        _info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage slot the transaction loaded but left unchanged.
    ///
    /// Only transaction-level sources report reads; sinks that persist changes can ignore them.
    #[inline]
    fn storage_read(
        &mut self,
        _address: Address,
        _key: Word,
        _value: Word,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Source of borrowed state changes.
pub trait StateChangeSource {
    /// Visits all changes. Ordering is source-defined and not guaranteed to be deterministic.
    ///
    /// Sources that track reads also report loaded-but-unchanged entries through
    /// [`StateChangeSink::account_read`] and [`StateChangeSink::storage_read`].
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error>;
}

/// Sink that ignores all changes.
#[derive(Clone, Debug, Default)]
#[allow(missing_copy_implementations)]
pub struct NoopChangeSink(());

impl StateChangeSink for NoopChangeSink {
    type Error = Infallible;
}

/// Sink that forwards each change to two sinks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tee<A, B> {
    a: A,
    b: B,
}

impl<A, B> Tee<A, B> {
    /// Creates a new tee sink.
    #[inline]
    pub const fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A, B> StateChangeSink for Tee<A, B>
where
    A: StateChangeSink,
    B: StateChangeSink<Error = A::Error>,
{
    type Error = A::Error;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.a.bytecode(code_hash, code)?;
        self.b.bytecode(code_hash, code)
    }

    #[inline]
    fn account_changes(&mut self, changes: AccountChanges<'_>) -> Result<(), Self::Error> {
        self.a.account_changes(changes)?;
        self.b.account_changes(changes)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.a.account(change)?;
        self.b.account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        self.a.storage_wipe(address)?;
        self.b.storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.a.storage(change)?;
        self.b.storage(change)
    }

    #[inline]
    fn account_read(
        &mut self,
        address: Address,
        info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        self.a.account_read(address, info)?;
        self.b.account_read(address, info)
    }

    #[inline]
    fn storage_read(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
    ) -> Result<(), Self::Error> {
        self.a.storage_read(address, key, value)?;
        self.b.storage_read(address, key, value)
    }
}
