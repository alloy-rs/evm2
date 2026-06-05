//! Tracked overlay values.

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
