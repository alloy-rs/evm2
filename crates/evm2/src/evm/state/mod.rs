//! Basic in-memory EVM host state.

mod account;
mod changes;
mod journal;
mod storage;
mod tracked;

use account::TrackedAccount;
pub use account::{Account, AccountEntry, AccountInfo};
pub use changes::{StateChanges, StorageChangeSet};
pub use journal::{JournalEntry, StateCheckpoint};
pub use storage::{StorageOverlay, StorageSlot, StorageSlotEntry, StoragesEntry};
pub use tracked::Tracked;
use tracked::TrackedAccountMap;

use super::{
    PrewarmSet,
    db::{CacheDB, DatabaseCommit, DbErrorCode, DbResult, DynDatabase},
    eip7708_burn_log,
};
use crate::{
    EvmFeatures, Version,
    bytecode::Bytecode,
    interpreter::{InstrStop, Word},
    storage_key::{StorageKey, StorageKeyMap},
};
use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY, Log,
    map::{AddressMap, AddressSet, hash_map},
};
use core::mem;
use core::ops::{Deref, DerefMut};
use derive_where::derive_where;

/// Mutable EVM state with an accepted-state cache, transaction layer, and reversible journal.
#[derive(Debug)]
#[non_exhaustive]
pub struct State {
    /// Account writes plus touch and warm-access metadata for the current transaction.
    accounts: TrackedAccountMap,
    /// Persistent storage writes plus warm slot metadata for the current transaction.
    storage: AddressMap<StorageOverlay>,
    /// Transaction-scoped EIP-1153 transient storage keyed by account address and slot.
    transient_storage: StorageKeyMap<Word>,
    /// Inner state.
    inner: StateInner,
}

impl Deref for State {
    type Target = StateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for State {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Shared inner state borrowed by journaled mutation handles.
///
/// Holds the parts of [`State`] that a [`AccountEntry`] or [`StoragesEntry`] needs while it
/// borrows an account or storage overlay: the backing database, the revert journal, and the
/// pre-warmed set. Splitting these out of [`State`] lets a handle borrow them
/// together as one `&mut StateInner` disjointly from the account/storage maps it mutates. [`State`]
/// derefs to this type, so its fields and methods are reachable directly on a [`State`].
#[derive_where(Debug)]
#[non_exhaustive]
pub struct StateInner {
    /// Database plus accepted transaction-boundary state overlay.
    #[derive_where(skip)]
    database: CacheDB<Box<dyn DynDatabase>>,
    /// Pre-warmed set: precompiles, coinbase, and the EIP-2930 access list.
    prewarm_set: PrewarmSet,
    /// Revert journal.
    journal: Vec<JournalEntry>,
    /// Logs emitted by the current transaction.
    logs: Vec<Log>,
    /// Accounts self-destructed in the current transaction.
    selfdestructs: AddressSet,
}

impl State {
    /// Creates a new state over an initial database.
    pub fn new(initial: impl DynDatabase) -> Self {
        Self::new_mono(Box::new(initial))
    }

    pub(crate) fn new_mono(initial: Box<dyn DynDatabase>) -> Self {
        Self {
            accounts: TrackedAccountMap::default(),
            storage: AddressMap::default(),
            transient_storage: StorageKeyMap::default(),
            inner: StateInner {
                database: CacheDB::new(initial),
                prewarm_set: PrewarmSet::new(),
                journal: Vec::new(),
                logs: Vec::new(),
                selfdestructs: AddressSet::default(),
            },
        }
    }

    /// Returns a checkpoint for later rollback.
    #[inline]
    pub const fn checkpoint(&self) -> StateCheckpoint {
        StateCheckpoint { journal_len: self.inner.journal.len(), logs_len: self.inner.logs.len() }
    }

    /// Returns the initial database.
    #[inline]
    pub fn initial(&self) -> &dyn DynDatabase {
        self.database.db.as_ref()
    }

    /// Returns the initial database mutably.
    #[inline]
    pub fn initial_mut(&mut self) -> &mut dyn DynDatabase {
        self.database.db.as_mut()
    }

    /// Replaces the initial database and clears all in-memory state layers.
    #[inline]
    pub fn set_initial(&mut self, initial: impl DynDatabase) {
        self.database = CacheDB::new(Box::new(initial));
        self.clear_transaction_state();
    }

    /// Loads a historical block hash.
    #[inline]
    pub(crate) fn block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        self.database.get_block_hash(number)
    }

    /// Returns logs emitted by the current in-flight transaction.
    #[inline]
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Records a transaction log.
    #[inline]
    pub fn log(&mut self, log: Log) {
        self.logs.push(log);
    }

    /// Returns a loaded persistent storage overlay slot, if present.
    ///
    /// This is a non-mutating overlay lookup. It does not load the account or slot from the
    /// backing database; use [`Self::storage`] when database-backed loading is desired.
    #[inline]
    pub fn storage_lookup(&self, address: &Address, key: &Word) -> Option<Word> {
        if let Some(storage) = self.storage.get(address) {
            if let Some(slot) = storage.slots.get(key).and_then(|slot| slot.value.as_ref()) {
                return Some(slot.current);
            }
            if storage.wiped {
                return Some(Word::ZERO);
            }
        }
        self.database.storage_ref(address, key)
    }

    /// Returns the current transaction account overlay if present and not deleted.
    ///
    /// This is a non-mutating overlay lookup. It does not load the account from the backing
    /// database; use [`Self::peek_account_info`] or [`Self::find`] when database-backed loading is
    /// desired.
    #[inline]
    #[must_use]
    pub fn account_lookup(&self, address: &Address) -> Option<&Account> {
        self.accounts.get(address)?.present.as_ref()
    }

    /// Returns the pre-warmed set (precompiles, coinbase, access list).
    ///
    /// This is not the complete EIP-2929 initial warm set — sender and recipient are warmed per
    /// account instead. See [`PrewarmSet`].
    #[inline]
    #[must_use]
    pub const fn prewarmset(&self) -> &PrewarmSet {
        &self.inner.prewarm_set
    }

    /// Returns the pre-warmed set mutably so callers can install precompiles, coinbase, the
    /// EIP-2930 access list, or non-revertible base warm accounts/slots.
    ///
    /// Entries added through this handle survive [`Self::rollback`] and are cleared per transaction
    /// by [`Self::clear_transaction_state`] (precompiles persist across transactions).
    #[inline]
    pub const fn prewarmset_mut(&mut self) -> &mut PrewarmSet {
        &mut self.inner.prewarm_set
    }

    /// Replaces the pre-warmed set wholesale.
    ///
    /// Use [`PrewarmSet`]'s builder methods to construct the set. The installed set survives
    /// [`Self::rollback`] and is cleared per transaction by [`Self::clear_transaction_state`].
    #[inline]
    pub fn set_prewarm_set(&mut self, prewarm_set: PrewarmSet) {
        self.inner.prewarm_set = prewarm_set;
    }

    /// Marks an account as warm in a revertible execution context, returning whether it was cold.
    ///
    /// If this call newly warms the account, the warm-set change is journaled and will be undone by
    /// [`Self::rollback`]. Use this for warmth introduced while executing EVM code or any other
    /// scope whose effects may be reverted to a checkpoint.
    ///
    /// Unlike [`Self::account_entry`] followed by [`AccountEntry::warm`], this marks warmth
    /// **without loading the account** from the backing database, so a caller that detects a cold
    /// access under `skip_cold_load` can bail before paying for the cold read. That no-load fast
    /// path is why this remains a dedicated method rather than going through a loaded handle.
    #[inline(never)]
    #[must_use]
    pub fn warm_account(&mut self, address: &Address) -> bool {
        if self.prewarm_set.is_warm(address) {
            return false;
        }
        if self.accounts.warm_account(*address) {
            self.journal.push(JournalEntry::AccountWarmed { address: *address });
            true
        } else {
            false
        }
    }

    /// Marks a storage slot as warm in a revertible execution context.
    ///
    /// Returns whether the slot was cold before this access. If this call newly warms the slot, the
    /// warm-set change is journaled and will be undone by [`Self::rollback`]. Use this for warmth
    /// introduced while executing EVM code or any other scope whose effects may be reverted to a
    /// checkpoint.
    #[inline(never)]
    #[must_use]
    pub fn warm_storage(&mut self, address: &Address, key: &Word) -> bool {
        self.storage_entry(address).slot(*key).warm()
    }

    /// Clears transaction-scoped substate.
    pub fn clear_transaction_state(&mut self) {
        self.accounts.clear();
        self.prewarm_set.clear_per_transaction();
        self.storage.clear();
        self.journal.clear();
        self.selfdestructs.clear();
        self.transient_storage.clear();
        self.logs.clear();
    }

    fn load_account(&mut self, address: &Address) -> DbResult<Option<Account>> {
        Ok(self.database.get_account(address)?.map(Account::from_info))
    }

    fn ensure_transaction_account<'a>(
        inner: &mut StateInner,
        accounts: &'a mut TrackedAccountMap,
        address: &Address,
    ) -> DbResult<&'a mut TrackedAccount> {
        Self::ensure_transaction_account_skip_cold(inner, accounts, address, false)
    }

    /// Ensures the account is present in the transaction overlay, loading it from the backing
    /// database when it has not been loaded yet.
    ///
    /// When `skip_cold` is true and the account is not already loaded, the cold database read is
    /// skipped and [`DbErrorCode::COLD_LOAD_SKIPPED`] is returned, leaving the overlay untouched.
    /// This mirrors revm's `skip_cold_load`/`ColdLoadSkipped` so callers can detect a cold access
    /// without paying for the load. With `skip_cold` false (or when the account is already loaded)
    /// the entry is always returned.
    fn ensure_transaction_account_skip_cold<'a>(
        inner: &mut StateInner,
        accounts: &'a mut TrackedAccountMap,
        address: &Address,
        skip_cold: bool,
    ) -> DbResult<&'a mut TrackedAccount> {
        match accounts.entry(*address) {
            hash_map::Entry::Occupied(entry) => {
                let entry = entry.into_mut();
                if !entry.is_loaded {
                    if skip_cold {
                        return Err(DbErrorCode::COLD_LOAD_SKIPPED);
                    }
                    let original = inner.database.get_account(address)?;
                    let present = original.clone().map(Account::from_info);
                    entry.original = original;
                    entry.present = present;
                    entry.is_loaded = true;
                    inner.journal.push(JournalEntry::AccountInserted { address: *address });
                }
                Ok(entry)
            }
            hash_map::Entry::Vacant(entry) => {
                if skip_cold {
                    return Err(DbErrorCode::COLD_LOAD_SKIPPED);
                }
                let original = inner.database.get_account(address)?;
                let present = original.clone().map(Account::from_info);
                inner.journal.push(JournalEntry::AccountInserted { address: *address });
                Ok(entry.insert(TrackedAccount {
                    original,
                    present,
                    is_loaded: true,
                    ..TrackedAccount::default()
                }))
            }
        }
    }

    /// Gets an existing account or inserts a new empty account.
    pub fn get_or_insert(&mut self, address: &Address) -> DbResult<&mut Account> {
        let entry = Self::ensure_transaction_account(&mut self.inner, &mut self.accounts, address)?;
        if entry.present.is_none() {
            self.inner
                .journal
                .push(JournalEntry::AccountChange { address: *address, previous: None });
            entry.present = Some(Account { code_hash: KECCAK256_EMPTY, ..Account::default() });
        }
        Ok(entry.present.as_mut().expect("account is inserted above"))
    }

    /// Loads `address` into the transaction overlay and returns a journaled mutation handle.
    ///
    /// Unlike [`Self::peek_account_info`], which reads the backing database without caching, this
    /// reads the account once and preserves it in the transaction overlay. The returned
    /// [`AccountEntry`] records a revert snapshot on its first mutation, so any changes made
    /// through it are undone together by [`Self::rollback`]. The account is materialized as empty
    /// only when it is first mutated while absent. This mirrors revm's `AccountEntry`.
    ///
    /// When `skip_cold_load` is true and the account has not been loaded into the overlay yet, the
    /// cold database read is skipped and [`DbErrorCode::COLD_LOAD_SKIPPED`] is returned, leaving the
    /// overlay untouched. Callers that cannot afford a cold access use this to detect it without
    /// paying for the load. An already-loaded account always yields a handle.
    pub fn account_entry(
        &mut self,
        address: &Address,
        skip_cold_load: bool,
    ) -> DbResult<AccountEntry<'_>> {
        let tracked = Self::ensure_transaction_account_skip_cold(
            &mut self.inner,
            &mut self.accounts,
            address,
            skip_cold_load,
        )?;
        Ok(AccountEntry::new(*address, tracked, &mut self.inner))
    }

    /// Returns a journaled mutation handle to `address`'s persistent storage overlay.
    ///
    /// The returned [`StoragesEntry`] ties the account's storage slots to the revert journal, so
    /// any slot warmed or written through it is undone together by [`Self::rollback`]. Slot values
    /// are read from the backing database lazily, only when a slot is loaded or first written. This
    /// mirrors [`Self::account_entry`] on the storage side.
    ///
    /// This does not load or touch the owning account; callers that need the account materialized
    /// must do so separately via [`Self::account_entry`].
    pub fn storage_entry(&mut self, address: &Address) -> StoragesEntry<'_> {
        let storage = self.storage.entry(*address).or_default();
        StoragesEntry::new(*address, storage, &mut self.inner)
    }

    /// Returns a journaled mutation handle to a single persistent storage slot of `address`.
    ///
    /// This is [`Self::storage_entry`] narrowed to one slot — a convenience for callers that need
    /// exactly one [`StorageSlotEntry`]. See [`StoragesEntry::slot`] for the per-slot semantics.
    pub fn storage_slot_entry(&mut self, address: &Address, key: Word) -> StorageSlotEntry<'_> {
        self.storage_entry(address).into_slot(key)
    }

    /// Returns account info from the overlay or the backing database.
    ///
    /// This is a non-loading peek: it returns the overlay account when one has been loaded this
    /// transaction, otherwise it reads the backing database directly without caching the result in
    /// the overlay. Use [`Self::account_entry`] when the account should be loaded and preserved.
    #[inline(never)]
    pub fn peek_account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        if let Some(present) =
            self.accounts.get(address).and_then(TrackedAccount::present_if_loaded)
        {
            return Ok(present.as_ref().map(Account::info));
        }
        self.database.get_account(address)
    }

    /// Transfers value between accounts.
    pub fn transfer(&mut self, from: &Address, to: &Address, value: &Word) -> DbResult<bool> {
        if value.is_zero() {
            self.account_entry(to, false)?.touch();
            return Ok(true);
        }

        if from == to {
            let mut account = self.account_entry(from, false)?;
            if account.balance() < *value {
                return Ok(false);
            }
            account.touch();
            return Ok(true);
        }

        {
            let mut from_account = self.account_entry(from, false)?;
            let Some(new_from_balance) = from_account.balance().checked_sub(*value) else {
                return Ok(false);
            };
            // `set_balance` touches the account, matching the touch the prior `transfer` performed.
            from_account.set_balance(new_from_balance);
        }
        {
            let mut to_account = self.account_entry(to, false)?;
            let new_to_balance = to_account.balance().saturating_add(*value);
            to_account.set_balance(new_to_balance);
        }
        Ok(true)
    }

    /// Creates a contract account and transfers endowment from the caller.
    #[inline(never)]
    pub fn create_account(
        &mut self,
        caller: &Address,
        address: &Address,
        value: &Word,
        features: EvmFeatures,
    ) -> DbResult<Result<(), InstrStop>> {
        if self
            .account_entry(address, false)?
            .get()
            .is_some_and(|account| account.nonce != 0 || account.code_hash != KECCAK256_EMPTY)
        {
            return Ok(Err(InstrStop::CreateCollision));
        }

        // Deduct the endowment from the caller. A zero endowment moves nothing and leaves the
        // caller untouched, matching the prior `transfer` behaviour.
        if !value.is_zero() {
            let mut caller_account = self.account_entry(caller, false)?;
            let Some(new_caller_balance) = caller_account.balance().checked_sub(*value) else {
                return Ok(Err(InstrStop::OutOfFunds));
            };
            caller_account.set_balance(new_caller_balance);
        }

        self.storage_entry(address).wipe();

        let mut target = self.account_entry(address, false)?;
        // Preserve any balance the address already held (e.g. funds sent before creation) and add
        // the endowment.
        let balance = target.balance().wrapping_add(*value);
        *target.get_or_insert() = Account {
            nonce: u64::from(features.contains(EvmFeatures::EIP161)),
            balance,
            code_hash: KECCAK256_EMPTY,
            code: Bytecode::default(),
            just_created: true,
            code_changed: true,
            _non_exhaustive: (),
        };
        target.touch();
        Ok(Ok(()))
    }

    /// Loads transient (EIP-1153) storage.
    #[must_use]
    pub fn tload(&mut self, address: &Address, key: &Word) -> Word {
        self.transient_storage.get(&StorageKey::new(*address, *key)).copied().unwrap_or_default()
    }

    /// Stores transient (EIP-1153) storage.
    pub fn tstore(&mut self, address: &Address, key: &Word, value: &Word) {
        match self.transient_storage.entry(StorageKey::new(*address, *key)) {
            hash_map::Entry::Occupied(mut entry) => {
                let previous = *entry.get();
                if previous == *value {
                    return;
                }
                self.inner.journal.push(JournalEntry::TransientStorageChange {
                    address: *address,
                    key: *key,
                    previous: Some(previous),
                });
                if value.is_zero() {
                    entry.remove();
                } else {
                    *entry.get_mut() = *value;
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if value.is_zero() {
                    return;
                }
                self.inner.journal.push(JournalEntry::TransientStorageChange {
                    address: *address,
                    key: *key,
                    previous: None,
                });
                entry.insert(*value);
            }
        }
    }

    /// Reverts state changes after the checkpoint.
    #[inline(never)]
    pub fn rollback(&mut self, checkpoint: StateCheckpoint, features: EvmFeatures) {
        assert!(checkpoint.journal_len <= self.journal.len(), "checkpoint is past journal length");
        assert!(checkpoint.logs_len <= self.logs.len(), "checkpoint is past logs length");
        self.logs.truncate(checkpoint.logs_len);
        while self.journal.len() != checkpoint.journal_len {
            let Some(entry) = self.journal.pop() else {
                unreachable!("checkpoint is checked above")
            };
            match entry {
                JournalEntry::AccountChange { address, previous } => {
                    if let Some(entry) = self.accounts.get_mut(&address) {
                        entry.present = previous;
                    }
                }
                JournalEntry::AccountInserted { address } => {
                    // Undo the database load that inserted this entry, then drop the entry unless
                    // it still carries warm or touched metadata recorded before the load.
                    if let hash_map::Entry::Occupied(mut entry) = self.accounts.entry(address) {
                        let account = entry.get_mut();
                        account.is_loaded = false;
                        account.original = None;
                        account.present = None;
                        if account.is_empty() {
                            entry.remove();
                        }
                    }
                }
                JournalEntry::Touch { address } => {
                    // EIP-161 preserves the historical Yellow Paper K.1 precompile-3 touch.
                    if features.contains(EvmFeatures::EIP161)
                        && address == Address::with_last_byte(3)
                    {
                        continue;
                    }
                    if let Some(entry) = self.accounts.get_mut(&address) {
                        entry.is_touched = false;
                    }
                }
                JournalEntry::SelfDestruct { address } => {
                    self.selfdestructs.remove(&address);
                }
                JournalEntry::StorageChange { address, key, previous } => {
                    if let Some(storage) = self.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                        && let Some(value) = slot.value.as_mut()
                    {
                        value.current = previous;
                    }
                }
                JournalEntry::StorageInserted { address, key } => {
                    // Undo the slot insert, then drop the slot (and the account's overlay when it
                    // is left empty and un-wiped) unless warm metadata keeps it alive.
                    if let hash_map::Entry::Occupied(mut storage_entry) = self.storage.entry(address)
                    {
                        let storage = storage_entry.get_mut();
                        if let Some(slot) = storage.slots.get_mut(&key) {
                            slot.value = None;
                            if slot.is_empty() {
                                storage.slots.remove(&key);
                            }
                        }
                        if !storage.wiped && storage.slots.is_empty() {
                            storage_entry.remove();
                        }
                    }
                }
                JournalEntry::StorageWipe { address, previous } => match previous {
                    Some(storage) => {
                        self.storage.insert(address, storage);
                    }
                    None => {
                        self.storage.remove(&address);
                    }
                },
                JournalEntry::TransientStorageChange { address, key, previous } => match previous {
                    Some(previous) if !previous.is_zero() => {
                        self.transient_storage.insert(StorageKey::new(address, key), previous);
                    }
                    _ => {
                        self.transient_storage.remove(&StorageKey::new(address, key));
                    }
                },
                JournalEntry::AccountWarmed { address } => {
                    if let hash_map::Entry::Occupied(mut entry) = self.accounts.entry(address) {
                        entry.get_mut().is_warm = false;
                        if entry.get().is_empty() {
                            entry.remove();
                        }
                    }
                }
                JournalEntry::StorageWarmed { address, key } => {
                    if let Some(storage) = self.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                    {
                        slot.warm = false;
                    }
                }
            }
        }
    }

    fn delete_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        let entry = Self::ensure_transaction_account(&mut self.inner, &mut self.accounts, address)?;
        let previous = entry.present.clone();
        self.inner.journal.push(JournalEntry::AccountChange { address: *address, previous });
        entry.present = None;
        self.storage_entry(address).wipe();
        Ok(())
    }

    fn materialize_empty_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        let original_exists = self.load_account(address)?.is_some();
        let entry = Self::ensure_transaction_account(&mut self.inner, &mut self.accounts, address)?;
        if !original_exists && entry.present.is_none() {
            self.inner
                .journal
                .push(JournalEntry::AccountChange { address: *address, previous: None });
            entry.present = Some(Account { code_hash: KECCAK256_EMPTY, ..Account::default() });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn finalize_transaction_(&mut self, version: &Version) {
        self.finalize_transaction(version, |_| {}).unwrap();
    }

    /// Applies transaction-finalization account-lifetime rules to the overlay.
    ///
    /// This mutates the in-memory post-transaction state before it is serialized
    /// by [`Self::build_state_changes`]. Runtime records
    /// transaction substate such as touches and selfdestructs, while finalization
    /// turns that substate into account deletions, storage wipes, or pre-EIP-161
    /// empty-account materialization.
    ///
    /// The callback lets the EVM inspect logs synthesized during finalization without storing
    /// inspector state in [`State`].
    pub(super) fn finalize_transaction(
        &mut self,
        version: &Version,
        mut inspect_log: impl FnMut(&Log),
    ) -> DbResult<()> {
        let selfdestructs = mem::take(&mut self.selfdestructs);
        let touched: Vec<_> = self
            .accounts
            .iter()
            .filter_map(|(&address, entry)| entry.is_touched.then_some(address))
            .collect();

        let delayed_burn_logs =
            version.feature(EvmFeatures::EIP7708 | EvmFeatures::EIP7708_DELAYED_BURN);
        if delayed_burn_logs {
            let mut burned = Vec::new();
            for &address in &selfdestructs {
                if let Some(balance) = self
                    .accounts
                    .get(&address)
                    .and_then(|entry| entry.present.as_ref())
                    .map(|account| account.balance)
                    && !balance.is_zero()
                {
                    burned.push((address, balance));
                }
            }
            burned.sort_by_key(|(address, _)| *address);
            for (address, balance) in burned {
                if let Some(log) = eip7708_burn_log(&address, &balance) {
                    inspect_log(&log);
                    self.log(log);
                }
            }
        }

        for address in &selfdestructs {
            self.delete_account_for_finalization(address)?;
        }

        if version.feature(EvmFeatures::EIP161) {
            for address in &touched {
                // EIP-161 deletes touched dead accounts at transaction finalization.
                if self.account_entry(address, false)?.is_existing_dead() {
                    self.delete_account_for_finalization(address)?;
                }
            }
        } else {
            for address in &touched {
                // Before EIP-161, touching a non-existent account materializes it as empty.
                if !selfdestructs.contains(address) && !self.account_entry(address, false)?.exists() {
                    self.materialize_empty_account_for_finalization(address)?;
                }
            }
        }

        self.selfdestructs = selfdestructs;
        self.selfdestructs.clear();

        for address in touched {
            if let hash_map::Entry::Occupied(mut entry) = self.accounts.entry(address) {
                entry.get_mut().is_touched = false;
                if entry.get().is_empty() {
                    entry.remove();
                }
            }
        }
        Ok(())
    }

    /// Builds the state transition and takes emitted logs for the current transaction.
    ///
    /// This does not apply changes to the backing database, apply transaction-finalization rules,
    /// or advance the overlay to the next transaction. It does move transaction-local logs into the
    /// returned [`StateChanges`], since callers clear transaction-local state immediately after
    /// accepting or discarding the transaction.
    pub(crate) fn build_state_changes(&mut self) -> StateChanges {
        let mut changes =
            StateChanges { logs: core::mem::take(&mut self.logs), ..StateChanges::default() };

        for (&address, entry) in self.accounts.iter() {
            if !entry.is_loaded {
                continue;
            }
            let original = entry.original.as_ref();
            let current = entry.present.as_ref();
            let account_changed = match (original, current) {
                (Some(original), Some(current)) => {
                    original.balance != current.balance
                        || original.nonce != current.nonce
                        || original.code_hash != current.code_hash
                }
                (None, None) => false,
                _ => true,
            };
            if account_changed {
                changes.accounts.insert(
                    address,
                    Tracked {
                        original: original.cloned(),
                        current: current.map(Account::info),
                        _non_exhaustive: (),
                    },
                );
            }
            if let Some(account) = current {
                let code_hash = account.code_hash;
                if account.code_changed
                    && !account.code.is_empty()
                    && !code_hash.is_zero()
                    && code_hash != KECCAK256_EMPTY
                {
                    changes.code.insert(code_hash, account.code.clone());
                }
            }
        }

        for (&address, storage) in &self.storage {
            let mut set = StorageChangeSet {
                wipe: storage.wiped,
                slots: BTreeMap::new(),
                _non_exhaustive: (),
            };
            for (&key, slot) in &storage.slots {
                let Some(slot) = slot.value.as_ref() else {
                    continue;
                };
                if slot.original != slot.current && (!set.wipe || !slot.current.is_zero()) {
                    set.slots.insert(
                        key,
                        Tracked {
                            original: slot.original,
                            current: slot.current,
                            _non_exhaustive: (),
                        },
                    );
                }
            }
            if set.wipe || !set.slots.is_empty() {
                changes.storage.insert(address, set);
            }
        }

        changes
    }

    /// Builds and accepts the current transaction's state transition.
    pub(crate) fn accept_transaction(&mut self) -> StateChanges {
        let changes = self.build_state_changes();
        self.database.commit(&changes);
        self.accounts.clear();
        self.storage.clear();
        changes
    }

    /// Marks the current transaction's write layer as accepted state.
    ///
    /// This applies the transaction write-set to the accepted in-memory database overlay and clears
    /// the transaction layer. It does not write to the wrapped backing database; callers remain
    /// responsible for committing the emitted write-set.
    #[cfg(test)]
    pub(super) fn commit_transaction_overlay(&mut self) {
        let _ = self.accept_transaction();
    }
}
