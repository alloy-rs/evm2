//! Storage effects a transaction defers to its end: blind adds to slots it has not read, and
//! storage operations to run against the state it leaves.

use super::{DbResult, State};
use crate::{interpreter::Word, storage_key::StorageKeyMap};
use alloc::{sync::Arc, vec::Vec};
use alloy_primitives::Address;
use core::fmt;

/// The pending blind adds to one storage slot the transaction has not read. See
/// [`State::storage_add`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StorageDelta {
    /// Wrapping sum of the blind adds.
    pub(crate) delta: Word,
    /// Whether a blind add warmed the slot for EIP-2929 gas accounting.
    pub(crate) warm: bool,
}

/// A transaction's storage effects deferred to its end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeferredStorage {
    /// Pending blind adds, by slot.
    pub(crate) deltas: StorageKeyMap<StorageDelta>,
    /// Deferred storage operations, in the order they were deferred.
    pub(crate) ops: Vec<DeferredStorageOp>,
    /// Slots the transaction's end loaded to resolve blind adds or run deferred operations: its
    /// execution never read them, so it does not depend on their earlier writers.
    pub(crate) resolved: Vec<(Address, Word)>,
}

impl DeferredStorage {
    /// Returns whether blind adds or deferred operations are left to resolve.
    #[inline]
    pub(crate) fn is_pending(&self) -> bool {
        !self.deltas.is_empty() || !self.ops.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.deltas.clear();
        self.ops.clear();
        self.resolved.clear();
    }
}

/// A storage operation deferred to the end of a transaction with [`State::defer_storage_op`].
///
/// It runs once the transaction finished executing, after its blind adds resolved, against the
/// state the transaction leaves, so its reads and writes are never part of the transaction's
/// execution: whatever it reads, the transaction's gas, output and other writes are the same.
/// Parallel block execution runs it when the transaction commits, against the state of every
/// transaction before it, so transactions that each update one shared slot this way, such as a
/// ring buffer's write pointer, do not conflict.
#[derive(Clone)]
pub struct DeferredStorageOp(Arc<DeferredStorageFn>);

/// The function a [`DeferredStorageOp`] runs.
type DeferredStorageFn = dyn Fn(&mut DeferredStorageAccess<'_, '_>) -> DbResult<()> + Send + Sync;

impl DeferredStorageOp {
    /// Creates a deferred storage operation.
    pub fn new(
        op: impl Fn(&mut DeferredStorageAccess<'_, '_>) -> DbResult<()> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(op))
    }

    /// Runs the operation against `state`.
    pub(crate) fn run(&self, state: &mut State<'_>) -> DbResult<()> {
        (self.0)(&mut DeferredStorageAccess { state })
    }
}

impl fmt::Debug for DeferredStorageOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeferredStorageOp").finish_non_exhaustive()
    }
}

/// Operations are equal when they are the same operation.
impl PartialEq for DeferredStorageOp {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for DeferredStorageOp {}

/// Storage access of a running [`DeferredStorageOp`]: reads and writes persistent storage slots
/// of the state the transaction leaves, without gas.
pub struct DeferredStorageAccess<'s, 'a> {
    state: &'s mut State<'a>,
}

impl fmt::Debug for DeferredStorageAccess<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeferredStorageAccess").finish_non_exhaustive()
    }
}

impl DeferredStorageAccess<'_, '_> {
    /// Reads a storage slot.
    pub fn sload(&mut self, address: &Address, key: Word) -> DbResult<Word> {
        self.note(address, key);
        Ok(self.state.storage_slot(address, key)?.current())
    }

    /// Writes a storage slot.
    pub fn sstore(&mut self, address: &Address, key: Word, value: Word) -> DbResult<()> {
        self.note(address, key);
        self.state.storage_slot(address, key)?.set(value);
        Ok(())
    }

    /// Records a slot the transaction's execution did not load.
    fn note(&mut self, address: &Address, key: Word) {
        if !self.state.is_storage_loaded(address, &key) {
            self.state.inner.deferred.resolved.push((*address, key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        evm::{
            CacheDB,
            state::{AccountInfo, Tracked},
        },
    };

    const ADDRESS: Address = Address::repeat_byte(0x44);

    fn state() -> State<'static> {
        let mut database = CacheDB::default();
        database.insert_account_info(&ADDRESS, AccountInfo::default());
        database.insert_account_storage(&ADDRESS, &Word::ONE, &Word::from(10));
        State::new(database)
    }

    fn version() -> &'static Version {
        Version::base(SpecId::PRAGUE)
    }

    /// Loads, increments and writes a slot as `SLOAD` and `SSTORE` do, returning whether the
    /// access was cold.
    fn read_add_write(state: &mut State<'_>, key: Word, delta: Word) -> bool {
        let (mut slot, is_cold) =
            state.storage(&ADDRESS).unwrap().into_slot_with_skip_and_warm(key, false).unwrap();
        slot.set(slot.current().wrapping_add(delta));
        is_cold
    }

    #[test]
    fn storage_add_matches_read_add_write() {
        let adds = [(Word::ONE, Word::from(5)), (Word::from(2), Word::MAX), (Word::ONE, Word::MAX)];
        let mut expected = state();
        let mut actual = state();
        for (key, delta) in adds {
            let value = actual.storage_slot_untracked(&ADDRESS, &key).unwrap();
            let is_cold = read_add_write(&mut expected, key, delta);
            assert_eq!(actual.storage_add(&ADDRESS, key, delta).unwrap(), is_cold);
            // The slot stays unloaded, but reads see the add.
            assert!(!actual.is_storage_loaded(&ADDRESS, &key));
            assert_eq!(
                actual.storage_slot_untracked(&ADDRESS, &key).unwrap(),
                value.wrapping_add(delta)
            );
        }
        expected.finalize_transaction_(version());
        actual.finalize_transaction_(version());
        let (expected, actual) = (expected.take_pending_state(), actual.take_pending_state());
        assert_eq!(actual.accounts, expected.accounts);
        assert!(!actual.has_deferred_storage());
        let mut resolved = actual.deferred.resolved;
        resolved.sort_unstable();
        assert_eq!(resolved, [(ADDRESS, Word::ONE), (ADDRESS, Word::from(2))]);
    }

    #[test]
    fn storage_add_reverts_with_its_frame() {
        let features = version().features;
        let mut expected = state();
        let mut actual = state();
        read_add_write(&mut expected, Word::ONE, Word::ONE);
        actual.storage_add(&ADDRESS, Word::ONE, Word::ONE).unwrap();
        let (expected_checkpoint, actual_checkpoint) = (expected.checkpoint(), actual.checkpoint());
        read_add_write(&mut expected, Word::ONE, Word::from(2));
        read_add_write(&mut expected, Word::from(2), Word::from(3));
        assert!(!actual.storage_add(&ADDRESS, Word::ONE, Word::from(2)).unwrap());
        assert!(actual.storage_add(&ADDRESS, Word::from(2), Word::from(3)).unwrap());
        expected.rollback(expected_checkpoint, features);
        actual.rollback(actual_checkpoint, features);

        assert_eq!(actual.storage_slot_untracked(&ADDRESS, &Word::ONE).unwrap(), Word::from(11));
        assert_eq!(actual.storage_slot_untracked(&ADDRESS, &Word::from(2)).unwrap(), Word::ZERO);
        assert!(!actual.storage(&ADDRESS).unwrap().is_warm(&Word::from(2)));
        expected.finalize_transaction_(version());
        actual.finalize_transaction_(version());
        // The reverted add leaves its slot loaded and unchanged, as a reverted read-add-write does.
        assert_eq!(actual.take_pending_state().accounts, expected.take_pending_state().accounts);
    }

    #[test]
    fn reading_a_slot_applies_its_pending_adds() {
        let features = version().features;
        let mut state = state();
        let start = state.checkpoint();
        assert!(state.storage_add(&ADDRESS, Word::ONE, Word::from(5)).unwrap());
        let before_read = state.checkpoint();
        let (slot, is_cold) = state
            .storage(&ADDRESS)
            .unwrap()
            .into_slot_with_skip_and_warm(Word::ONE, false)
            .unwrap();
        assert_eq!(
            (slot.original(), slot.current(), is_cold),
            (Word::from(10), Word::from(15), false)
        );

        // Reverting the read leaves the add pending over the loaded slot.
        state.rollback(before_read, features);
        assert!(!state.storage_add(&ADDRESS, Word::ONE, Word::ONE).unwrap());
        assert_eq!(state.storage_slot(&ADDRESS, Word::ONE).unwrap().current(), Word::from(16));

        state.rollback(start, features);
        let slot = state.storage_slot(&ADDRESS, Word::ONE).unwrap();
        assert_eq!(slot.current(), Word::from(10));
        assert!(!slot.is_warm());
    }

    #[test]
    fn deferred_storage_resolves_against_the_committing_state() {
        let pointer = Word::from(3);
        let mut worker = state();
        worker.set_defer_storage_resolution(true);
        worker.storage_add(&ADDRESS, Word::ONE, Word::from(5)).unwrap();
        worker.defer_storage_op(DeferredStorageOp::new(move |storage| {
            let value = storage.sload(&ADDRESS, pointer)?;
            storage.sstore(&ADDRESS, pointer, value + Word::ONE)
        }));
        worker.finalize_transaction_(version());
        let mut pending = worker.take_pending_state();
        assert!(pending.has_deferred_storage());
        assert!(pending.accounts[&ADDRESS].storage.slots.is_empty());

        // Earlier transactions committed other values meanwhile.
        let mut committed = state();
        committed.storage_slot(&ADDRESS, Word::ONE).unwrap().set(Word::from(20));
        committed.storage_slot(&ADDRESS, pointer).unwrap().set(Word::from(7));
        committed.commit_transaction();
        committed.clear_transaction_state();
        committed.resolve_pending_state(&mut pending).unwrap();

        assert!(!pending.has_deferred_storage());
        let slots = &pending.accounts[&ADDRESS].storage.slots;
        assert_eq!(slots[&Word::ONE].value, Tracked::from_parts(Word::from(20), Word::from(25)));
        assert_eq!(slots[&pointer].value, Tracked::from_parts(Word::from(7), Word::from(8)));
        assert_eq!(pending.deferred.resolved, [(ADDRESS, Word::ONE), (ADDRESS, pointer)]);
        assert!(committed.journal().is_empty());
    }

    #[test]
    fn deferred_storage_ops_run_at_the_end_and_revert_with_their_frame() {
        let mut state = state();
        let write = |key: u64| {
            DeferredStorageOp::new(move |storage| {
                storage.sstore(&ADDRESS, Word::from(key), Word::from(key + 1))
            })
        };
        state.defer_storage_op(write(5));
        let checkpoint = state.checkpoint();
        state.defer_storage_op(write(6));
        state.rollback(checkpoint, version().features);
        assert!(!state.is_storage_loaded(&ADDRESS, &Word::from(5)));

        state.finalize_transaction_(version());
        let pending = state.take_pending_state();
        let slots = &pending.accounts[&ADDRESS].storage.slots;
        assert_eq!(slots[&Word::from(5)].value, Tracked::from_parts(Word::ZERO, Word::from(6)));
        assert!(!slots.contains_key(&Word::from(6)));
    }
}
