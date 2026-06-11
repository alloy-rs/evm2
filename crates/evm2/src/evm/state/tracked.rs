//! Tracked overlay values.

use super::TrackedAccount;
use alloy_primitives::{Address, map::AddressMap};
use core::ops::{Deref, DerefMut};

/// A value tracked together with the value it had at an aggregation boundary.
///
/// `Tracked` is used by [`State`](super::State) to keep a transaction overlay over the backing
/// database, by [`StateChanges`](super::StateChanges) to describe transaction transitions, and by
/// [`BlockStateAccumulator`](super::BlockStateAccumulator) to describe block transitions.
/// `original` is the value at the start of the current transaction or block, while `current` is the
/// value after all observed mutations in that boundary. When a transaction is accepted, `current`
/// becomes the next transaction's `original` without writing anything to the backing database.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Tracked<T> {
    /// Value at the start of the aggregation boundary.
    pub original: T,
    /// Value after observed mutations in the aggregation boundary.
    pub current: T,
}

impl<T> Tracked<T> {
    /// Creates a tracked value from distinct original and current values.
    #[inline]
    pub const fn from_parts(original: T, current: T) -> Self {
        Self { original, current }
    }

    /// Creates a tracked value whose original and current values are equal.
    #[inline]
    pub fn new(value: T) -> Self
    where
        T: Clone,
    {
        Self { original: value.clone(), current: value }
    }

    /// Updates the current value.
    #[inline]
    pub fn set_current(&mut self, current: T) {
        self.current = current;
    }

    /// Splits this tracked value into original and current values.
    #[inline]
    pub fn into_parts(self) -> (T, T) {
        (self.original, self.current)
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
