//! Tracked overlay values.

use super::TrackedAccount;
use alloy_primitives::{Address, map::AddressMap};
use core::ops::{Deref, DerefMut};

/// A value tracked together with the value it had at the start of the current
/// transaction.
///
/// `Tracked` is used by [`State`](super::State) to keep an overlay over the
/// backing database and by [`StateChanges`](super::StateChanges) to describe
/// account and storage transitions. `original` is the value at the current
/// transaction boundary, while `current` is the value after all in-flight EVM
/// mutations. When a transaction is accepted, `current` becomes the next
/// transaction's `original` without writing anything to the backing database.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Tracked<T> {
    /// Value at the start of the current transaction.
    pub original: T,
    /// Current overlay value.
    pub current: T,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T> Tracked<T> {
    /// Creates a tracked value whose original and current values are equal.
    #[inline]
    pub fn new(value: T) -> Self
    where
        T: Clone,
    {
        Self { original: value.clone(), current: value, _non_exhaustive: () }
    }
}

impl<T: PartialEq> Tracked<T> {
    /// Returns whether the current value differs from the original value.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.original != self.current
    }
}

/// Revm-style account access list for the current transaction.
///
/// This is the single account-side transaction map: account overlays, touched-account state, and
/// EIP-2929 account warmth all live in the same entry. A warm or touched account does not have to
/// be loaded from the database, matching revm's separation between warm access metadata and
/// database-backed account loads.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TrackedAccountMap {
    accounts: AddressMap<TrackedAccount>,
}

impl TrackedAccountMap {
    /// Marks the account as warm, inserting an entry if needed.
    ///
    /// Returns `true` if the account was previously cold.
    #[inline]
    pub(super) fn warm_account(&mut self, address: Address) -> bool {
        let entry = self.accounts.entry(address).or_default();
        let was_cold = !entry.is_warm;
        entry.is_warm = true;
        was_cold
    }
}

impl Deref for TrackedAccountMap {
    type Target = AddressMap<TrackedAccount>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.accounts
    }
}

impl DerefMut for TrackedAccountMap {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.accounts
    }
}
