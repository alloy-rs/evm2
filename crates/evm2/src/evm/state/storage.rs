//! Transaction-scoped persistent storage overlay.

use super::{DbErrorCode, DbResult, DynDatabase, JournalEntry, StateInner, Tracked};
use crate::interpreter::Word;
use alloy_primitives::{Address, map::U256Map};
use derive_where::derive_where;

/// Persistent storage overlay for one account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageOverlay {
    /// Whether consumers must delete all pre-existing storage for the account
    /// before applying individual slot changes.
    pub wiped: bool,
    /// Loaded, changed, or warmed storage slots.
    pub slots: U256Map<StorageSlot>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Persistent storage slot metadata cached by [`super::State`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageSlot {
    /// Loaded storage value, if the slot value has been read or written.
    pub value: Option<Tracked<Word>>,
    /// Whether this storage slot is warm in the current transaction.
    pub warm: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StorageSlot {
    #[inline]
    pub(super) const fn is_empty(&self) -> bool {
        self.value.is_none() && !self.warm
    }

    #[inline]
    pub(super) const fn mark_warm(&mut self) -> bool {
        let was_cold = !self.warm;
        self.warm = true;
        was_cold
    }
}

/// A mutable, journaled handle to one account's persistent storage overlay.
///
/// Returned by [`State::storage_entry`](super::State::storage_entry). It ties the account's
/// [`StorageOverlay`] to the revert journal, the backing database, and the transaction-initial base
/// warm set, mirroring [`AccountEntry`](super::AccountEntry) on the storage side: a slot
/// mutation and its rollback bookkeeping cannot drift apart.
///
/// Individual slots are reached through [`Self::slot`], which yields a [`StorageSlotEntry`]
/// scoped to one key. The handle itself records nothing; journaling happens per-slot when a slot is
/// warmed or written.
#[derive_where(Debug)]
pub struct StoragesEntry<'a> {
    /// Address of the account whose storage this handle exposes.
    address: Address,
    /// Transaction overlay entry: the per-account storage slots plus the wipe flag.
    storage: &'a mut StorageOverlay,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner,
}

impl<'a> StoragesEntry<'a> {
    /// Creates a handle over an account's storage overlay and the shared inner state (backing
    /// database, revert journal, and transaction-initial base warm set).
    #[inline]
    pub(super) const fn new(
        address: Address,
        storage: &'a mut StorageOverlay,
        inner: &'a mut StateInner,
    ) -> Self {
        Self { address, storage, inner }
    }

    /// Returns the account address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns whether the account's storage is marked for a full wipe before individual slot
    /// changes are applied.
    #[inline]
    pub const fn is_wiped(&self) -> bool {
        self.storage.wiped
    }

    /// Returns a journaled handle to the storage slot at `key`, inserting an empty overlay slot
    /// when it has not been touched yet.
    ///
    /// The returned [`StorageSlotEntry`] reborrows this handle, so it cannot outlive it; hold
    /// the storage handle and call this repeatedly to operate on several slots.
    #[inline]
    pub fn slot(&mut self, key: Word) -> StorageSlotEntry<'_> {
        let wiped = self.storage.wiped;
        let slot = self.storage.slots.entry(key).or_default();
        StorageSlotEntry { address: self.address, key, slot, inner: &mut *self.inner, wiped }
    }

    /// Consumes the handle and returns a journaled handle to the storage slot at `key` for the
    /// full borrow, inserting an empty overlay slot when it has not been touched yet.
    ///
    /// Unlike [`Self::slot`], which reborrows, this hands the underlying overlay and inner-state
    /// borrows to the returned [`StorageSlotEntry`], letting it outlive this handle. Used by
    /// [`State::storage_slot_entry`](super::State::storage_slot_entry) to reach a single slot
    /// directly.
    #[inline]
    pub fn into_slot(self, key: Word) -> StorageSlotEntry<'a> {
        let wiped = self.storage.wiped;
        let slot = self.storage.slots.entry(key).or_default();
        StorageSlotEntry { address: self.address, key, slot, inner: self.inner, wiped }
    }

    /// Marks all of the account's prior persistent storage as deleted.
    ///
    /// The overlay is replaced by a wiped one that keeps only the warm-access metadata of
    /// previously warmed slots, so EIP-2929 warmth survives the wipe while their values resolve to
    /// zero. A [`JournalEntry::StorageWipe`] snapshot of the prior overlay is recorded so the wipe
    /// is undone by [`State::rollback`](super::State::rollback).
    #[inline]
    pub fn wipe(&mut self) {
        let previous = self.storage.clone();
        let mut wiped =
            StorageOverlay { wiped: true, slots: U256Map::default(), _non_exhaustive: () };
        for (&key, slot) in &previous.slots {
            if slot.warm {
                wiped
                    .slots
                    .insert(key, StorageSlot { value: None, warm: true, _non_exhaustive: () });
            }
        }
        *self.storage = wiped;
        self.inner
            .journal
            .push(JournalEntry::StorageWipe { address: self.address, previous: Some(previous) });
    }
}

/// A mutable, journaled handle to a single persistent storage slot.
///
/// Returned by [`StoragesEntry::slot`]. Warming the slot records a
/// [`JournalEntry::StorageWarmed`], and writing it records a [`JournalEntry::StorageChange`] or
/// [`JournalEntry::StorageInserted`], so every effect made through the handle is undone together by
/// [`State::rollback`](super::State::rollback). A handle used only for reads records nothing.
///
/// The handle carries the shared [`StateInner`] (backing database, revert journal, and base warm
/// set) and the account's wipe flag so it can journal effects and resolve the slot's
/// transaction-boundary original value on demand without going back through
/// [`State`](super::State).
#[derive_where(Debug)]
pub struct StorageSlotEntry<'a> {
    /// Address of the account that owns the slot.
    address: Address,
    /// Storage key of the slot.
    key: Word,
    /// Transaction overlay entry: the slot value plus its warm-access metadata.
    slot: &'a mut StorageSlot,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner,
    /// Whether the owning account's storage is wiped, so cold reads resolve to zero.
    wiped: bool,
}

impl StorageSlotEntry<'_> {
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

    /// Returns the tracked slot value, or `None` when the slot has not been loaded or written.
    #[inline]
    pub const fn get(&self) -> Option<&Tracked<Word>> {
        self.slot.value.as_ref()
    }

    /// Returns the current slot value, or `None` when the slot has not been loaded or written.
    #[inline]
    pub fn current(&self) -> Option<Word> {
        self.slot.value.as_ref().map(|tracked| tracked.current)
    }

    /// Returns the slot's transaction-boundary original value, or `None` when the slot has not been
    /// loaded or written.
    #[inline]
    pub fn original(&self) -> Option<Word> {
        self.slot.value.as_ref().map(|tracked| tracked.original)
    }

    /// Returns whether the slot is warm for EIP-2929 gas accounting, consulting both the
    /// transaction's base warm set (EIP-2930 access-list slots) and runtime warmth recorded during
    /// execution.
    #[inline]
    pub fn is_warm(&self) -> bool {
        self.slot.warm || self.inner.prewarm_set.is_storage_warm(&self.address, &self.key)
    }

    /// Marks the slot warm for EIP-2929 gas accounting, recording a [`JournalEntry::StorageWarmed`]
    /// when this access transitions it from cold to warm.
    ///
    /// Returns `true` if the slot was cold before this call. Slots already warm through the base
    /// warm set stay warm across rollback, so warming them again records nothing.
    #[inline]
    pub fn warm(&mut self) -> bool {
        if self.inner.prewarm_set.is_storage_warm(&self.address, &self.key) {
            return false;
        }
        if self.slot.mark_warm() {
            self.inner
                .journal
                .push(JournalEntry::StorageWarmed { address: self.address, key: self.key });
            true
        } else {
            false
        }
    }

    /// Loads the current slot value, reading it from the backing database and caching it in the
    /// overlay when it has not been loaded yet.
    ///
    /// A pure load records no revert entry: the cached value's original equals its current, so it
    /// is left in place by [`State::rollback`](super::State::rollback) as a harmless cache.
    ///
    /// When `skip_cold_load` is true and the slot has not been loaded into the overlay yet, the cold
    /// database read is skipped and [`DbErrorCode::COLD_LOAD_SKIPPED`] is returned, leaving the slot
    /// untouched. An already-loaded slot always returns its value.
    #[inline]
    pub fn load(&mut self, skip_cold_load: bool) -> DbResult<Word> {
        if let Some(tracked) = self.slot.value.as_ref() {
            return Ok(tracked.current);
        }
        if skip_cold_load {
            return Err(DbErrorCode::COLD_LOAD_SKIPPED);
        }
        let value = self.load_original()?;
        self.slot.value.get_or_insert(Tracked::new(value));
        Ok(value)
    }

    /// Sets the slot value, recording a revert entry when the value actually changes.
    ///
    /// The first write to a not-yet-loaded slot resolves its original value from the backing
    /// database and records a [`JournalEntry::StorageInserted`]; later writes record a
    /// [`JournalEntry::StorageChange`]. Writing the value the slot already holds records nothing.
    ///
    /// When `skip_cold_load` is true and the slot has not been loaded into the overlay yet, the cold
    /// database read needed to resolve its original value is skipped and
    /// [`DbErrorCode::COLD_LOAD_SKIPPED`] is returned, leaving the slot untouched.
    #[inline]
    pub fn set(&mut self, value: Word, skip_cold_load: bool) -> DbResult<()> {
        match self.slot.value {
            Some(ref mut tracked) => {
                let previous = tracked.current;
                if previous == value {
                    return Ok(());
                }
                tracked.current = value;
                self.inner.journal.push(JournalEntry::StorageChange {
                    address: self.address,
                    key: self.key,
                    previous,
                });
            }
            None => {
                if skip_cold_load {
                    return Err(DbErrorCode::COLD_LOAD_SKIPPED);
                }
                let original = self.load_original()?;
                self.slot.value = Some(Tracked::from_parts(original, value));
                self.inner
                    .journal
                    .push(JournalEntry::StorageInserted { address: self.address, key: self.key });
            }
        }
        Ok(())
    }

    /// Writes `value`, returning the slot's transaction-boundary original value and the value the
    /// slot held just before this write — the pair `SSTORE` net-gas metering needs.
    ///
    /// This is [`Self::load`] followed by a conditional [`Self::set`]: the original is resolved
    /// from the backing database on first access, and a revert entry is recorded only when the
    /// value actually changes. Passing `skip_cold_load` propagates
    /// [`DbErrorCode::COLD_LOAD_SKIPPED`] from the underlying load when the slot is cold.
    #[inline]
    pub fn write(&mut self, value: Word, skip_cold_load: bool) -> DbResult<(Word, Word)> {
        let present_value = self.load(skip_cold_load)?;
        let original_value = self.original().unwrap_or(present_value);
        if present_value != value {
            self.set(value, skip_cold_load)?;
        }
        Ok((original_value, present_value))
    }

    /// Resolves the slot's transaction-boundary value from the backing database, accounting for a
    /// wiped or absent account.
    #[inline]
    fn load_original(&mut self) -> DbResult<Word> {
        if self.wiped || self.inner.database.account_absent(&self.address) {
            return Ok(Word::ZERO);
        }
        self.inner.database.get_storage(&self.address, &self.key)
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
    use alloy_primitives::Address;

    #[test]
    fn storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &Word::from(1), &Word::from(10));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.storage_entry(&address).slot(Word::from(1)).write(Word::from(20), false).unwrap();
        state.storage_entry(&address).slot(Word::from(1)).write(Word::from(30), false).unwrap();

        assert_eq!(
            state.storage_entry(&address).slot(Word::from(1)).load(false).unwrap(),
            Word::from(30)
        );
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(
            state.storage_entry(&address).slot(Word::from(1)).load(false).unwrap(),
            Word::from(10)
        );
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
    fn storage_wipe_preserves_warm_slots_in_merged_storage_map() {
        let account = Address::with_last_byte(0x19);
        let warm_key = Word::from(1);
        let cold_key = Word::from(2);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&account, &warm_key, &Word::from(3));
        database.insert_account_storage(&account, &cold_key, &Word::from(4));
        let mut state = State::new(database);

        assert!(state.prewarmset_mut().warm_storage(&account, &warm_key));
        state.storage_entry(&account).slot(cold_key).write(Word::from(5), false).unwrap();

        state.storage_entry(&account).wipe();
        assert!(state.storage_slot_entry(&account, warm_key).is_warm());
        assert!(!state.storage_slot_entry(&account, cold_key).is_warm());
        assert_eq!(state.storage_lookup(&account, &warm_key), Some(Word::ZERO));
        assert_eq!(state.storage_lookup(&account, &cold_key), Some(Word::ZERO));

        let changes = state.build_state_changes();
        let storage = changes.storage.get(&account).expect("wipe must be emitted");
        assert!(storage.wipe);
        assert!(storage.slots.is_empty());
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
            let mut storage = state.storage_entry(&address);
            let mut slot = storage.slot(key);
            assert_eq!(slot.load(false).unwrap(), Word::from(10));
            assert_eq!(slot.original(), Some(Word::from(10)));
            assert!(slot.warm(), "first access is cold");
            assert!(!slot.warm(), "second access is warm");
            slot.set(Word::from(20), false).unwrap();
            slot.set(Word::from(30), false).unwrap();
        }

        assert!(state.storage_slot_entry(&address, key).is_warm());
        assert_eq!(state.storage_entry(&address).slot(key).load(false).unwrap(), Word::from(30));

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.storage_slot_entry(&address, key).is_warm());
        assert_eq!(state.storage_entry(&address).slot(key).load(false).unwrap(), Word::from(10));
        assert!(state.build_state_changes().is_empty());
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
            let mut storage = state.storage_entry(&address);
            let mut slot = storage.slot(key);
            assert_eq!(slot.load(false).unwrap(), Word::from(5));
            assert_eq!(slot.current(), Some(Word::from(5)));
        }
        // Loading caches the value but a read-only handle records no transition.
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.build_state_changes().is_empty());
    }

    #[test]
    fn journaled_storage_slot_skip_cold_load_signals_skip() {
        let address = Address::from([0x35; 20]);
        let key = Word::from(9);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &key, &Word::from(42));
        let mut state = State::new(database);

        let mut storage = state.storage_entry(&address);
        let mut slot = storage.slot(key);
        // A cold, not-yet-loaded slot signals the skip instead of reading the database.
        assert_eq!(slot.load(true), Err(DbErrorCode::COLD_LOAD_SKIPPED));
        assert_eq!(slot.set(Word::from(7), true), Err(DbErrorCode::COLD_LOAD_SKIPPED));
        assert_eq!(slot.current(), None, "skipping leaves the slot untouched");
        // Once loaded, skipping no longer applies.
        assert_eq!(slot.load(false).unwrap(), Word::from(42));
        assert_eq!(slot.load(true).unwrap(), Word::from(42));
    }
}
