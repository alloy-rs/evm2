//! BAL change types and the per-item change list.

use super::BlockAccessIndex;
use crate::bytecode::{Bytecode, BytecodeDecodeError};
use alloc::vec::Vec;
use alloy_eip7928::{BalanceChange, CodeChange as AlloyCodeChange, NonceChange, StorageChange};
use alloy_primitives::{B256, U256};

/// A single change of one state item at a [`BlockAccessIndex`].
///
/// Implemented on the EIP-7928 change types ([`NonceChange`], [`BalanceChange`],
/// [`StorageChange`]) and the internal [`BalCodeChange`] so [`BalChanges`] can share the
/// access and push logic across all of them.
pub trait BalChange {
    /// The post-state value carried by the change.
    type Value: PartialEq + Clone;

    /// The part of the value that [`BalChanges::update`] compares to detect a change.
    type Key: PartialEq + Clone;

    /// The comparison key of `value`.
    fn key(value: &Self::Value) -> &Self::Key;

    /// Create a new change.
    fn new(index: BlockAccessIndex, value: Self::Value) -> Self;

    /// The [`BlockAccessIndex`] at which the change happened.
    fn block_access_index(&self) -> BlockAccessIndex;

    /// The post-state value of the change.
    fn value(&self) -> &Self::Value;

    /// Replace the post-state value of the change.
    fn set_value(&mut self, value: Self::Value);
}

impl BalChange for NonceChange {
    type Value = u64;
    type Key = u64;

    fn key(value: &Self::Value) -> &Self::Key {
        value
    }

    fn new(index: BlockAccessIndex, value: Self::Value) -> Self {
        Self { block_access_index: index, new_nonce: value }
    }

    fn block_access_index(&self) -> BlockAccessIndex {
        self.block_access_index
    }

    fn value(&self) -> &Self::Value {
        &self.new_nonce
    }

    fn set_value(&mut self, value: Self::Value) {
        self.new_nonce = value;
    }
}

impl BalChange for BalanceChange {
    type Value = U256;
    type Key = U256;

    fn key(value: &Self::Value) -> &Self::Key {
        value
    }

    fn new(index: BlockAccessIndex, value: Self::Value) -> Self {
        Self { block_access_index: index, post_balance: value }
    }

    fn block_access_index(&self) -> BlockAccessIndex {
        self.block_access_index
    }

    fn value(&self) -> &Self::Value {
        &self.post_balance
    }

    fn set_value(&mut self, value: Self::Value) {
        self.post_balance = value;
    }
}

impl BalChange for StorageChange {
    type Value = U256;
    type Key = U256;

    fn key(value: &Self::Value) -> &Self::Key {
        value
    }

    fn new(index: BlockAccessIndex, value: Self::Value) -> Self {
        Self { block_access_index: index, new_value: value }
    }

    fn block_access_index(&self) -> BlockAccessIndex {
        self.block_access_index
    }

    fn value(&self) -> &Self::Value {
        &self.new_value
    }

    fn set_value(&mut self, value: Self::Value) {
        self.new_value = value;
    }
}

/// A code change carrying the decoded [`Bytecode`] and its cached hash.
///
/// The EIP-7928 [`AlloyCodeChange`] only stores raw bytes; keeping the decoded bytecode and
/// hash lets BAL reads populate account info without re-decoding and re-hashing, and keeps
/// bytecode validation at import time.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BalCodeChange {
    /// The [`BlockAccessIndex`] at which the code changed.
    pub block_access_index: BlockAccessIndex,
    /// The post-state code hash and decoded bytecode.
    pub code: (B256, Bytecode),
}

impl BalCodeChange {
    /// Create a new code change.
    pub const fn new(block_access_index: BlockAccessIndex, code: (B256, Bytecode)) -> Self {
        Self { block_access_index, code }
    }
}

impl BalChange for BalCodeChange {
    type Value = (B256, Bytecode);
    type Key = B256;

    fn key(value: &Self::Value) -> &Self::Key {
        &value.0
    }

    fn new(index: BlockAccessIndex, value: Self::Value) -> Self {
        Self { block_access_index: index, code: value }
    }

    fn block_access_index(&self) -> BlockAccessIndex {
        self.block_access_index
    }

    fn value(&self) -> &Self::Value {
        &self.code
    }

    fn set_value(&mut self, value: Self::Value) {
        self.code = value;
    }
}

impl TryFrom<&AlloyCodeChange> for BalCodeChange {
    type Error = BytecodeDecodeError;

    fn try_from(change: &AlloyCodeChange) -> Result<Self, Self::Error> {
        let bytecode = Bytecode::new_raw_checked(change.new_code().clone())?;
        let hash = bytecode.hash_slow();
        Ok(Self { block_access_index: change.block_access_index, code: (hash, bytecode) })
    }
}

impl From<BalCodeChange> for AlloyCodeChange {
    fn from(change: BalCodeChange) -> Self {
        Self::new(change.block_access_index, change.code.1.original_bytes())
    }
}

/// Chronological change list for one state item.
///
/// If empty it means that this item was read from database.
#[derive(Debug, Clone)]
pub struct BalChanges<T: BalChange> {
    /// List of changes, sorted by [`BlockAccessIndex`].
    pub changes: Vec<T>,
    /// Key of the value before the first recorded change. If the only change is at the current
    /// index, this is the value before that index, which a later commit at the same index can
    /// restore.
    prior: Option<T::Key>,
}

impl<T: BalChange> Default for BalChanges<T> {
    fn default() -> Self {
        Self { changes: Vec::new(), prior: None }
    }
}

impl<T: BalChange + PartialEq> PartialEq for BalChanges<T> {
    fn eq(&self, other: &Self) -> bool {
        self.changes == other.changes
    }
}

impl<T: BalChange + Eq> Eq for BalChanges<T> {}

impl<T: BalChange> From<Vec<T>> for BalChanges<T> {
    fn from(changes: Vec<T>) -> Self {
        Self { changes, prior: None }
    }
}

impl<T: BalChange> BalChanges<T> {
    /// Create a new BalChanges.
    pub fn new(mut changes: Vec<T>) -> Self {
        changes.sort_by_key(|change| change.block_access_index());
        Self { changes, prior: None }
    }

    /// Linear search is used for small number of changes. It is faster than binary search.
    #[inline(never)]
    pub fn get_linear_search(&self, bal_index: BlockAccessIndex) -> Option<&T::Value> {
        // find the first change at or after bal_index; the value before it is the one visible.
        let i = self
            .changes
            .iter()
            .position(|change| change.block_access_index() >= bal_index)
            .unwrap_or(self.changes.len());
        // only if i is not zero, we return the previous value.
        (i != 0).then(|| self.changes[i - 1].value())
    }

    /// Get value from BAL.
    pub fn get(&self, bal_index: BlockAccessIndex) -> Option<&T::Value> {
        if self.changes.len() < 5 {
            return self.get_linear_search(bal_index);
        }
        // else do binary search.
        let i = match self
            .changes
            .binary_search_by_key(&bal_index, |change| change.block_access_index())
        {
            Ok(i) => i,
            Err(i) => i,
        };
        // only if i is not zero, we return the previous value.
        (i != 0).then(|| self.changes[i - 1].value())
    }

    /// Extend the builder with another builder.
    pub fn extend(&mut self, other: Self) {
        self.changes.extend(other.changes);
    }

    /// Returns true if the builder is empty.
    pub const fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Force insert a value into the BalChanges.
    ///
    /// Check if last index is same as the index to insert.
    /// If it is, we override the value.
    /// If it is not, we push the value to the end of the vector.
    ///
    /// No checks for original value is done. This is useful when we know that value is different.
    #[inline]
    pub fn force_update(&mut self, index: BlockAccessIndex, value: T::Value) {
        if let Some(last) = self.changes.last_mut()
            && index == last.block_access_index()
        {
            last.set_value(value);
            return;
        }
        self.changes.push(T::new(index, value));
    }

    /// Insert a value into the builder.
    ///
    /// `original` is the key of the value before this update. If [`BlockAccessIndex`] is same as
    /// last it will override the value, or remove it if the value is back to the one before the
    /// index.
    ///
    /// Assumes that index is always greater than last one and that changes are updated in proper
    /// order.
    #[inline]
    pub fn update(&mut self, index: BlockAccessIndex, original: &T::Key, value: T::Value) {
        // A no-op update records nothing. It must also preserve a change recorded earlier at the
        // same index, since `original` is then the value that change set.
        if original == T::key(&value) {
            return;
        }

        // if index is different, we push the new value.
        if let Some(last) = self.changes.last_mut()
            && last.block_access_index() != index
        {
            // we push the new value only if it is changed.
            if T::key(last.value()) != T::key(&value) {
                self.changes.push(T::new(index, value));
            }
            return;
        }

        // extract the value before the index and the last change at the index.
        let (previous, last) = match self.changes.as_mut_slice() {
            [.., previous, last] => (T::key(previous.value()), last),
            // `original` is the value before this update, which can be the value set by an
            // earlier commit at this index. Compare against the value before the index instead.
            [last] => (self.prior.as_ref().unwrap_or(original), last),
            [] => {
                self.prior = Some(original.clone());
                self.changes.push(T::new(index, value));
                return;
            }
        };

        // if previous value is same, we pop the last value.
        if previous == T::key(&value) {
            self.changes.pop();
            return;
        }

        // if it is different, we update the last value.
        last.set_value(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloy_primitives::bytes;

    const fn idx(index: u64) -> BlockAccessIndex {
        BlockAccessIndex::new(index)
    }

    #[test]
    fn test_get() {
        let bal_changes = BalChanges::new(vec![
            NonceChange::new(idx(0), 1),
            NonceChange::new(idx(1), 2),
            NonceChange::new(idx(2), 3),
        ]);
        assert_eq!(bal_changes.get(idx(0)), None);
        assert_eq!(bal_changes.get(idx(1)), Some(&1));
        assert_eq!(bal_changes.get(idx(2)), Some(&2));
        assert_eq!(bal_changes.get(idx(3)), Some(&3));
        assert_eq!(bal_changes.get(idx(4)), Some(&3));
    }

    fn get_binary_search(threshold: u64) {
        // Construct test data up to (threshold - 1), skipping one key to simulate a gap.
        let entries: Vec<_> = (0..threshold - 1)
            .map(|i| NonceChange::new(idx(i), i + 1))
            .chain(core::iter::once(NonceChange::new(idx(threshold), threshold + 1)))
            .collect();

        let bal_changes = BalChanges::new(entries);

        // Case 1: lookup before any entries
        assert_eq!(bal_changes.get(idx(0)), None);

        // Case 2: lookups for existing keys before the gap
        for i in 1..threshold - 1 {
            assert_eq!(bal_changes.get(idx(i)), Some(&i));
        }

        // Case 3: lookup at the skipped key — should return the previous value
        assert_eq!(bal_changes.get(idx(threshold)), Some(&(threshold - 1)));

        // Case 4: lookup after the skipped key — should return the next valid value
        assert_eq!(bal_changes.get(idx(threshold + 1)), Some(&(threshold + 1)));
    }

    #[test]
    fn test_get_binary_search() {
        get_binary_search(4);
        get_binary_search(5);
        get_binary_search(6);
        get_binary_search(7);
    }

    #[test]
    fn update_preserves_existing_change_for_noop_at_same_index() {
        let mut changes = BalChanges::<NonceChange>::default();

        changes.update(idx(1), &0, 1);
        changes.update(idx(1), &1, 1);

        assert_eq!(changes.changes, vec![NonceChange::new(idx(1), 1)]);
    }

    #[test]
    fn update_overwrites_existing_change_at_same_index() {
        let mut changes = BalChanges::<NonceChange>::default();

        changes.update(idx(1), &0, 1);
        changes.update(idx(1), &1, 2);

        assert_eq!(changes.changes, vec![NonceChange::new(idx(1), 2)]);
    }

    #[test]
    fn update_drops_change_reverted_at_same_index() {
        // The first change in the block: the baseline is the value before the index, not the
        // original of the later commit.
        let mut changes = BalChanges::<StorageChange>::default();
        changes.update(idx(1), &U256::ZERO, U256::from(1));
        changes.update(idx(1), &U256::from(1), U256::ZERO);
        assert!(changes.is_empty());

        // An earlier change is the baseline.
        changes.update(idx(1), &U256::ZERO, U256::from(1));
        changes.update(idx(2), &U256::from(1), U256::from(2));
        changes.update(idx(2), &U256::from(2), U256::from(1));
        assert_eq!(changes.changes, vec![StorageChange::new(idx(1), U256::from(1))]);

        let code = Bytecode::new_raw(bytes!("6001"));
        let mut changes = BalChanges::<BalCodeChange>::default();
        changes.update(idx(1), &B256::ZERO, (code.hash_slow(), code.clone()));
        changes.update(idx(1), &code.hash_slow(), (B256::ZERO, Bytecode::default()));
        assert!(changes.is_empty());
    }
}
