//! Transaction-scoped persistent storage overlay.

use super::{Account, DbResult, DynDatabase, JournalEntry, StateInner, Tracked};
use crate::{LoadError, interpreter::Word};
use alloy_primitives::{
    Address,
    map::{U256Map, hash_map},
};
use derive_where::derive_where;

/// Persistent storage overlay for one account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageOverlay {
    /// Loaded storage slots. A slot is present here only once it has been loaded or written, so
    /// its value is always meaningful; EIP-2929 warmth is tracked per slot in
    /// [`StorageSlot::is_warm`].
    pub slots: U256Map<StorageSlot>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StorageOverlay {
    /// Applies an isolated execution's slots, retaining parent originals and combining warmth.
    #[cfg(test)]
    pub(crate) fn merge_isolated(&mut self, child: Self) {
        for (key, slot) in child.slots {
            match self.slots.entry(key) {
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(slot);
                }
                hash_map::Entry::Occupied(mut entry) => {
                    let parent = entry.get_mut();
                    parent.value.current = slot.value.current;
                    parent.is_warm |= slot.is_warm;
                }
            }
        }
    }

    /// Returns the changed storage slots.
    ///
    /// A slot is changed when its current value differs from its transaction-boundary original,
    /// except that resetting the owning account's storage must reinsert every nonzero current
    /// value. Set `reset` when the account lifecycle requires a full storage reset before these
    /// writes.
    #[inline]
    pub fn changed_slots(&self, reset: bool) -> impl Iterator<Item = (&Word, &Tracked<Word>)> {
        self.slots
            .iter()
            .filter_map(move |(key, slot)| slot.is_changed(reset).then_some((key, &slot.value)))
    }
}

/// Persistent storage slot cached by [`super::State`].
///
/// A slot is held in the overlay only once it has been loaded or written, so its value is always
/// present. [`Self::is_warm`] records runtime EIP-2929 warmth observed during execution;
/// base/access-list warmth is held in the prewarm set, not here.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageSlot {
    /// Tracked slot value: its transaction-boundary original together with the current value.
    pub value: Tracked<Word>,
    /// Whether the slot was warmed during execution this transaction, for EIP-2929 gas accounting.
    pub is_warm: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StorageSlot {
    /// Whether this slot must be emitted as a write, reinserting nonzero values after a wipe.
    #[inline]
    pub(super) fn is_changed(&self, reset: bool) -> bool {
        if reset { !self.value.current.is_zero() } else { self.value.is_changed() }
    }

    /// Creates a freshly loaded slot whose original and current values are `value`, with the given
    /// EIP-2929 warmth.
    #[inline]
    fn loaded(value: Word, is_warm: bool) -> Self {
        Self { value: Tracked::new(value), is_warm, _non_exhaustive: () }
    }
}

/// A mutable, journaled handle to one account's persistent storage overlay.
///
/// Returned by [`State::storage`](super::State::storage). It ties the account's
/// [`StorageOverlay`] to the revert journal, the backing database, and the transaction-initial base
/// warm set, mirroring [`AccountHandle`](super::AccountHandle) on the storage side: a slot
/// mutation and its rollback bookkeeping cannot drift apart.
///
/// An individual slot is reached through [`Self::into_slot`], which loads the slot and yields a
/// [`StorageSlotHandle`] scoped to one key. Warmth without loading is answered cheaply by
/// [`Self::is_warm`] / [`Self::is_loaded`], so callers can make EIP-2929 cold/warm decisions before
/// paying for a cold database read.
#[derive_where(Debug)]
pub struct StorageHandle<'a, 'db> {
    /// Address of the account whose storage this handle exposes.
    address: Address,
    /// Owning account, including its lifecycle state and loaded storage slots.
    account: &'a mut Account,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner<'db>,
}

impl<'a, 'db> StorageHandle<'a, 'db> {
    /// Creates a handle over an account's storage overlay and the shared inner state (backing
    /// database, revert journal, and transaction-initial base warm set).
    #[inline]
    pub(crate) const fn new(
        address: Address,
        account: &'a mut Account,
        inner: &'a mut StateInner<'db>,
    ) -> Self {
        Self { address, account, inner }
    }

    /// Returns the account address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns whether the slot at `key` has already been loaded into the overlay this transaction.
    ///
    /// This is a pure overlay membership check: it does not consult the backing database or load
    /// anything, so callers can use it to detect a cold slot before paying for a load.
    #[inline]
    pub fn is_loaded(&self, key: &Word) -> bool {
        self.account.storage.slots.contains_key(key)
    }

    /// Returns whether the slot at `key` is warm for EIP-2929 gas accounting, consulting both the
    /// transaction's base warm set (EIP-2930 access-list slots) and runtime warmth recorded on an
    /// already-loaded slot.
    ///
    /// Like [`Self::is_loaded`], this neither loads the slot nor reads the backing database, so it
    /// can gate a cold access before the cold read is paid for.
    #[inline]
    pub fn is_warm(&self, key: &Word) -> bool {
        self.account.storage.slots.get(key).is_some_and(|slot| slot.is_warm)
            || self.inner.prewarm_set.is_storage_warm(&self.address, key)
    }

    /// Loads a storage slot without skipping cold accesses and returns a journaled handle.
    ///
    /// See [`Self::into_slot_with_skip`] for the loading and journaling semantics.
    #[inline]
    pub fn into_slot(self, key: Word) -> DbResult<StorageSlotHandle<'a, 'db>> {
        self.into_slot_with_skip(key, false).map_err(|error| match error {
            LoadError::Database(error) => error,
            LoadError::ColdLoadSkipped => unreachable!("cold-load skipping is disabled"),
        })
    }

    /// Loads the slot at `key` into the overlay, reading the backing database on first access, and
    /// returns a journaled handle to it.
    ///
    /// When `skip_cold_load` is true and the slot is cold, the access
    /// is skipped and [`LoadError::ColdLoadSkipped`] is returned, leaving the overlay
    /// untouched. This mirrors [`State::account_with_skip`](super::State::account_with_skip)'s
    /// `skip_cold_load`/`ColdLoadSkipped` so callers can detect a cold access without paying for
    /// the load. Warm slots are loaded even when not yet present in the overlay.
    ///
    /// A slot is materialized in the overlay only once it is loaded, so the returned
    /// [`StorageSlotHandle`] always refers to a slot with a meaningful value. On first load the
    /// slot's [`StorageSlot::is_warm`] is seeded from the base warm set (EIP-2930 access list),
    /// mirroring how [`account_raw`](super::State) seeds an account's warmth. The load itself
    /// records no revert entry: the cached value's original equals its current, so it is left in
    /// place by [`State::rollback`](super::State::rollback) as a harmless cache. Used by
    /// [`State::storage_slot`](super::State::storage_slot) to reach a single slot directly.
    #[inline]
    pub fn into_slot_with_skip(
        self,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<StorageSlotHandle<'a, 'db>, LoadError> {
        self.into_slot_inner::<false>(key, skip_cold_load).map(|(slot, _)| slot)
    }

    /// Loads a storage slot, marks it warm, and returns whether this access was cold.
    ///
    /// This combines slot loading and EIP-2929 warmth handling so callers do not need to consult
    /// the base warm set once while loading and again while warming the returned handle.
    #[inline]
    pub fn into_slot_with_skip_and_warm(
        self,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<(StorageSlotHandle<'a, 'db>, bool), LoadError> {
        self.into_slot_inner::<true>(key, skip_cold_load)
    }

    #[inline]
    fn into_slot_inner<const WARM: bool>(
        self,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<(StorageSlotHandle<'a, 'db>, bool), LoadError> {
        let Self { address, account, inner } = self;
        let reset = account.storage_is_reset(false);
        let is_destroyed = account.is_destroyed;
        let (slot, is_cold) = match account.storage.slots.entry(key) {
            hash_map::Entry::Occupied(entry) => {
                // An already-loaded slot has no cold database read to skip, so the skip only
                // signals an unaffordable *cold* access. Runtime warmth (`is_warm`, seeded from
                // the prewarm set on load) decides coldness: a slot warmed earlier this execution
                // is a cheap warm access and must not be forced out of gas.
                let slot = entry.into_mut();
                let is_cold = if WARM || skip_cold_load {
                    !slot.is_warm && !inner.prewarm_set.is_storage_warm(&address, &key)
                } else {
                    false
                };
                if skip_cold_load && is_cold {
                    return Err(LoadError::ColdLoadSkipped);
                }
                (slot, is_cold)
            }
            hash_map::Entry::Vacant(entry) => {
                let is_warm = inner.prewarm_set.is_storage_warm(&address, &key);
                if skip_cold_load && !is_warm {
                    return Err(LoadError::ColdLoadSkipped);
                }
                // SELFDESTRUCT keeps storage readable until finalization. EIP-8246 clears
                // the live destroyed flag while retaining the transaction set membership.
                let value = if reset || (!is_destroyed && inner.selfdestructs.contains(&address)) {
                    Word::ZERO
                } else {
                    inner.database.get_storage(&address, &key)?
                };
                (entry.insert(StorageSlot::loaded(value, is_warm)), !is_warm)
            }
        };
        if WARM && is_cold {
            slot.is_warm = true;
            inner.journal.push(JournalEntry::StorageWarmed { address, key });
        }
        Ok((StorageSlotHandle { address, key, slot, inner }, is_cold))
    }
}

/// A mutable, journaled handle to a single, loaded persistent storage slot.
///
/// Returned by [`StorageHandle::into_slot`]. Warming the slot records a
/// [`JournalEntry::StorageWarmed`] and writing it records a [`JournalEntry::StorageChange`], so
/// every effect made through the handle is undone together by
/// [`State::rollback`](super::State::rollback). A handle used only for reads records nothing.
///
/// The handle holds a mutable reference to the slot's overlay entry — which exists only because the
/// slot has been loaded, so its value is always meaningful — together with the shared
/// [`StateInner`] (backing database, revert journal, and base warm set) needed to journal effects
/// and answer warm-access queries.
#[derive_where(Debug)]
pub struct StorageSlotHandle<'a, 'db> {
    /// Address of the account that owns the slot.
    address: Address,
    /// Storage key of the slot.
    key: Word,
    /// The slot's loaded overlay entry: its tracked value and runtime warmth.
    slot: &'a mut StorageSlot,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner<'db>,
}

impl StorageSlotHandle<'_, '_> {
    /// Returns the account address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns the storage key.
    #[inline]
    pub const fn key(&self) -> Word {
        self.key
    }

    /// Returns the tracked slot value.
    #[inline]
    pub const fn get(&self) -> &Tracked<Word> {
        &self.slot.value
    }

    /// Returns the current slot value.
    #[inline]
    pub const fn current(&self) -> Word {
        self.slot.value.current
    }

    /// Returns the slot's transaction-boundary original value.
    #[inline]
    pub const fn original(&self) -> Word {
        self.slot.value.original
    }

    /// Returns whether the slot is warm for EIP-2929 gas accounting, consulting both the
    /// transaction's base warm set (EIP-2930 access-list slots) and runtime warmth recorded during
    /// execution.
    #[inline]
    pub fn is_warm(&self) -> bool {
        self.slot.is_warm || self.inner.prewarm_set.is_storage_warm(&self.address, &self.key)
    }

    /// Marks the slot warm for EIP-2929 gas accounting, recording a [`JournalEntry::StorageWarmed`]
    /// when this access transitions it from cold to warm.
    ///
    /// Returns `true` if the slot was cold before this call. Slots already warm through the base
    /// warm set stay warm across rollback, so warming them again records nothing.
    #[inline]
    pub fn warm(&mut self) -> bool {
        if self.is_warm() {
            return false;
        }
        self.slot.is_warm = true;
        self.inner
            .journal
            .push(JournalEntry::StorageWarmed { address: self.address, key: self.key });
        true
    }

    /// Sets the slot value, recording a [`JournalEntry::StorageChange`] when the value actually
    /// changes. Writing the value the slot already holds records nothing.
    #[inline]
    pub fn set(&mut self, value: Word) {
        let previous = self.slot.value.current;
        if previous == value {
            return;
        }
        self.slot.value.current = value;
        self.inner.journal.push(JournalEntry::StorageChange {
            address: self.address,
            key: self.key,
            previous,
        });
    }

    /// Writes `value`, returning the slot's transaction-boundary original value and the value the
    /// slot held just before this write — the pair `SSTORE` net-gas metering needs.
    ///
    /// A revert entry is recorded only when the value actually changes, via [`Self::set`].
    #[inline]
    pub fn write(&mut self, value: Word) -> (Word, Word) {
        let original_value = self.slot.value.original;
        let present_value = self.slot.value.current;
        self.set(value);
        (original_value, present_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        evm::{
            CacheDB,
            state::{AccountInfo, State},
        },
    };
    use alloc::vec;
    use alloy_primitives::Address;

    #[test]
    fn storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &Word::from(1), &Word::from(10));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.storage_slot(&address, Word::from(1)).unwrap().write(Word::from(20));
        state.storage_slot(&address, Word::from(1)).unwrap().write(Word::from(30));

        assert_eq!(state.storage_slot(&address, Word::from(1)).unwrap().current(), Word::from(30));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.storage_slot(&address, Word::from(1)).unwrap().current(), Word::from(10));
    }

    #[test]
    fn transient_storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x22; 20]);
        let mut state = State::new(CacheDB::default());

        state.tstore(&address, &Word::from(1), &Word::from(10));
        let checkpoint = state.checkpoint();
        state.tstore(&address, &Word::from(1), &Word::from(20));

        assert_eq!(state.tload(&address, &Word::from(1)), Word::from(20));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.tload(&address, &Word::from(1)), Word::from(10));
    }

    #[test]
    fn take_transient_storage_only_drains_requested_account() {
        let address = Address::from([0x22; 20]);
        let other = Address::from([0x23; 20]);
        let mut state = State::new(CacheDB::default());

        state.tstore(&address, &Word::from(1), &Word::from(10));
        state.tstore(&address, &Word::from(2), &Word::from(20));
        state.tstore(&other, &Word::from(1), &Word::from(30));

        let mut slots = state.take_transient_storage(&address);
        slots.sort_unstable_by_key(|(key, _)| *key);
        assert_eq!(slots, vec![(Word::from(1), Word::from(10)), (Word::from(2), Word::from(20))]);
        assert_eq!(state.tload(&address, &Word::from(1)), Word::ZERO);
        assert_eq!(state.tload(&other, &Word::from(1)), Word::from(30));
    }

    #[test]
    fn storage_wipe_preserves_warm_slots_in_merged_storage_map() {
        let account = Address::with_last_byte(0x19);
        let warm_key = Word::from(1);
        let cold_key = Word::from(2);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&account, &warm_key, &Word::from(3));
        database.insert_account_storage(&account, &cold_key, &Word::from(4));
        let mut state = State::new(database);

        state.prewarm_storage_slot(&account, warm_key);
        state.storage_slot(&account, cold_key).unwrap().write(Word::from(5));

        state.account(&account).unwrap().mark_destructed();
        state.finalize_transaction_(Version::base(SpecId::LONDON));
        assert!(state.storage_slot(&account, warm_key).unwrap().is_warm());
        assert!(!state.storage_slot(&account, cold_key).unwrap().is_warm());
        assert_eq!(state.storage_slot(&account, warm_key).unwrap().current(), Word::ZERO);
        assert_eq!(state.storage_slot(&account, cold_key).unwrap().current(), Word::ZERO);

        let pending = state.take_pending_state();
        let account = pending.accounts.get(&account).expect("deletion must be emitted");
        assert!(account.present.is_none());
        assert!(account.storage.changed_slots(true).next().is_none());
    }

    #[test]
    fn journaled_storage_mutations_journal_and_roll_back() {
        let address = Address::from([0x33; 20]);
        let key = Word::from(1);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &key, &Word::from(10));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let mut slot = state.storage_slot(&address, key).unwrap();
            assert_eq!(slot.current(), Word::from(10));
            assert_eq!(slot.original(), Word::from(10));
            assert!(slot.warm(), "first access is cold");
            assert!(!slot.warm(), "second access is warm");
            slot.set(Word::from(20));
            slot.set(Word::from(30));
        }

        assert!(state.storage_slot(&address, key).unwrap().is_warm());
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::from(30));

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.storage_slot(&address, key).unwrap().is_warm());
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::from(10));
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn journaled_storage_read_only_handle_journals_nothing() {
        let address = Address::from([0x34; 20]);
        let key = Word::from(7);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &key, &Word::from(5));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let slot = state.storage_slot(&address, key).unwrap();
            assert_eq!(slot.current(), Word::from(5));
        }
        // Loading caches the value but a read-only handle records no transition.
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn fused_slot_load_warms_once_and_reverts() {
        let address = Address::from([0x35; 20]);
        let key = Word::from(8);
        let mut state = State::new(CacheDB::default());
        let checkpoint = state.checkpoint();

        assert!(matches!(
            state.storage(&address).unwrap().into_slot_with_skip_and_warm(key, true),
            Err(LoadError::ColdLoadSkipped)
        ));
        assert!(!state.storage(&address).unwrap().is_loaded(&key));

        let (slot, is_cold) =
            state.storage(&address).unwrap().into_slot_with_skip_and_warm(key, false).unwrap();
        assert!(is_cold);
        assert_eq!(slot.current(), Word::ZERO);
        assert!(state.storage_slot(&address, key).unwrap().is_warm());

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.storage_slot(&address, key).unwrap().is_warm());
    }

    #[test]
    fn fused_slot_load_honors_prewarm_added_after_loading() {
        let address = Address::with_last_byte(1);
        let key = Word::from(8);
        let mut state = State::new(CacheDB::default());
        state.storage_slot(&address, key).unwrap();
        state.prewarm_storage_slot(&address, key);
        let checkpoint = state.checkpoint();

        let (slot, is_cold) =
            state.storage(&address).unwrap().into_slot_with_skip_and_warm(key, true).unwrap();
        assert!(!is_cold);
        assert!(slot.is_warm());
        assert_eq!(state.checkpoint(), checkpoint);
    }

    #[test]
    fn storage_handle_reports_warmth_before_loading() {
        let address = Address::from([0x35; 20]);
        let key = Word::from(9);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &key, &Word::from(42));
        let mut state = State::new(database);

        // A cold, not-yet-loaded slot reports neither loaded nor warm without touching the
        // database.
        assert!(!state.storage(&address).unwrap().is_loaded(&key));
        assert!(!state.storage(&address).unwrap().is_warm(&key));

        // Loading materializes the slot with its database value; warming marks it warm.
        {
            let mut slot = state.storage_slot(&address, key).unwrap();
            assert_eq!(slot.current(), Word::from(42));
            assert!(slot.warm(), "first access is cold");
            assert!(!slot.warm(), "second access is warm");
        }
        assert!(state.storage(&address).unwrap().is_loaded(&key));
        assert!(state.storage(&address).unwrap().is_warm(&key));
    }

    #[test]
    fn storage_slot_warm_honors_prewarm_after_load() {
        let address = Address::from([0x36; 20]);
        let key = Word::from(11);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &key, &Word::from(42));
        let mut state = State::new(database);

        // Load the slot while cold, then extend the base prewarm set. The handle's pure warmth
        // query and its mutating transition must agree that the slot is already warm.
        assert!(!state.storage_slot(&address, key).unwrap().is_warm());
        state.prewarm_storage_slot(&address, key);

        let mut slot = state.storage_slot(&address, key).unwrap();
        assert!(slot.is_warm());
        assert!(!slot.warm(), "base-prewarmed slot must not report a cold transition");
    }

    #[test]
    fn created_storage_uses_zero_base_and_commits_reset() {
        let address = Address::with_last_byte(0x40);
        let loaded = Word::ONE;
        let unseen = Word::from(2);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        database.insert_account_storage(&address, &loaded, &Word::from(10));
        database.insert_account_storage(&address, &unseen, &Word::from(20));
        let mut state = State::new(database.clone());
        let version = Version::base(SpecId::LONDON);
        state
            .create_account(&Address::ZERO, &address, &Word::ZERO, version.features)
            .unwrap()
            .unwrap();

        assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::ZERO);
        {
            let mut account = state.account(&address).unwrap();
            let mut slot = account.storage().into_slot(loaded).unwrap();
            assert_eq!(slot.original(), Word::ZERO);
            assert_eq!(slot.current(), Word::ZERO);
            slot.set(Word::from(30));
        }
        state.finalize_transaction_(version);
        let pending = state.take_pending_state();

        // All three commit paths must replace the backing storage and keep constructor writes.
        let mut detached = CacheDB::new(database.clone());
        detached.commit_pending(&pending);
        let mut streamed = CacheDB::new(database);
        streamed.commit_source(&pending);
        state.set_pending_state(pending);
        state.commit_transaction();
        for cache in [&mut detached, &mut streamed] {
            assert_eq!(cache.get_storage(&address, &loaded).unwrap(), Word::from(30));
            assert_eq!(cache.get_storage(&address, &unseen).unwrap(), Word::ZERO);
        }
        assert_eq!(state.storage_slot_untracked(&address, &loaded).unwrap(), Word::from(30));
        assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::ZERO);
    }

    #[test]
    fn selfdestruct_preserves_storage_until_finalization() {
        let address = Address::with_last_byte(0x41);
        let loaded = Word::ONE;
        let unseen = Word::from(2);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        database.insert_account_storage(&address, &loaded, &Word::from(10));
        database.insert_account_storage(&address, &unseen, &Word::from(20));
        let mut state = State::new(database);
        let version = Version::base(SpecId::LONDON);
        let checkpoint = state.checkpoint();
        state.account(&address).unwrap().mark_destructed();
        assert_eq!(state.storage_slot(&address, loaded).unwrap().current(), Word::from(10));
        assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::from(20));
        state.rollback(checkpoint, version.features);
        assert!(!state.account(&address).unwrap().is_destructed());
        assert!(!state.take_pending_state().is_changed());

        state.account(&address).unwrap().mark_destructed();
        state.storage_slot(&address, loaded).unwrap().set(Word::from(30));
        state.finalize_transaction_(version);
        assert_eq!(state.storage_slot(&address, loaded).unwrap().current(), Word::ZERO);
        assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::ZERO);
        assert_eq!(state.storage_slot(&address, unseen).unwrap().current(), Word::ZERO);
        state.commit_transaction();
        assert_eq!(state.storage_slot_untracked(&address, &loaded).unwrap(), Word::ZERO);
        assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::ZERO);
    }

    #[test]
    fn finalized_account_lifecycle_resets_detached_storage() {
        // Also cover EIP-8246 with an existing account to exercise the retained selfdestruct
        // set independently of creation, as custom feature combinations can do.
        for spec in [SpecId::LONDON, SpecId::SPURIOUS_DRAGON, SpecId::AMSTERDAM] {
            let address = Address::with_last_byte(0x42);
            let loaded = Word::ONE;
            let unseen = Word::from(2);
            let info = if spec == SpecId::SPURIOUS_DRAGON {
                AccountInfo::default()
            } else {
                AccountInfo::default().with_balance(Word::ONE)
            };
            let mut database = CacheDB::default();
            database.insert_account_info(&address, info);
            database.insert_account_storage(&address, &loaded, &Word::from(10));
            database.insert_account_storage(&address, &unseen, &Word::from(20));
            let mut state = State::new(database.clone());
            state.storage_slot(&address, loaded).unwrap().set(Word::from(30));
            if spec == SpecId::SPURIOUS_DRAGON {
                state.account(&address).unwrap().touch();
            } else {
                state.account(&address).unwrap().mark_destructed();
            }
            state.finalize_transaction_(Version::base(spec));
            let pending = state.take_pending_state();
            let account = &pending.accounts[&address];
            assert!(!account.is_created());
            if spec == SpecId::AMSTERDAM {
                assert!(!account.is_destroyed);
                assert_eq!(account.present, Some(AccountInfo::default().with_balance(Word::ONE)));
            } else {
                assert!(account.present.is_none());
            }

            let mut detached = CacheDB::new(database.clone());
            detached.commit_pending(&pending);
            let mut streamed = CacheDB::new(database);
            streamed.commit_source(&pending);
            state.set_pending_state(pending);
            assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::ZERO);
            assert_eq!(state.storage_slot(&address, unseen).unwrap().current(), Word::ZERO);
            state.commit_transaction();
            for cache in [&mut detached, &mut streamed] {
                assert_eq!(cache.get_storage(&address, &loaded).unwrap(), Word::ZERO);
                assert_eq!(cache.get_storage(&address, &unseen).unwrap(), Word::ZERO);
            }
            assert_eq!(state.storage_slot_untracked(&address, &loaded).unwrap(), Word::ZERO);
            assert_eq!(state.storage_slot_untracked(&address, &unseen).unwrap(), Word::ZERO);
        }
    }

    #[test]
    fn reverted_creation_does_not_reset_backing_storage() {
        let address = Address::with_last_byte(0x43);
        let key = Word::ONE;
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        database.insert_account_storage(&address, &key, &Word::from(10));
        let mut state = State::new(database);
        let version = Version::base(SpecId::LONDON);
        let checkpoint = state.checkpoint();
        state
            .create_account(&Address::ZERO, &address, &Word::ZERO, version.features)
            .unwrap()
            .unwrap();
        assert_eq!(state.storage_slot_untracked(&address, &key).unwrap(), Word::ZERO);
        state.rollback(checkpoint, version.features);
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::from(10));
        let pending = state.take_pending_state();
        assert!(!pending.is_changed());
        state.set_pending_state(pending);
        state.commit_transaction();
        assert_eq!(state.storage_slot_untracked(&address, &key).unwrap(), Word::from(10));
    }
}
