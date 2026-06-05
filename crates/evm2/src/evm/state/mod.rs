//! Basic in-memory EVM host state.

mod account;
mod changes;
mod journal;
mod storage;
mod tracked;

use account::TrackedAccount;
pub use account::{Account, AccountInfo, JournaledAccount};
pub use changes::{StateChanges, StorageChangeSet};
pub use journal::{JournalEntry, StateCheckpoint};
pub use storage::{StorageOverlay, StorageSlot};
use tracked::TrackedAccountMap;
pub use tracked::Tracked;

use super::{
    SStore, WarmAddresses,
    db::{CacheDB, DatabaseCommit, DbResult, DynDatabase},
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
    map::{AddressMap, AddressSet, HashSet, U256Map, hash_map},
};
use core::mem;
use derive_where::derive_where;

/// Mutable EVM state with an accepted-state cache, transaction layer, and reversible journal.
#[derive_where(Debug)]
#[non_exhaustive]
pub struct State {
    /// Database plus accepted transaction-boundary state overlay.
    #[derive_where(skip)]
    database: CacheDB<Box<dyn DynDatabase>>,
    /// Account writes plus touch and warm-access metadata for the current transaction.
    accounts: TrackedAccountMap,
    /// Transaction-initial base warm set: precompiles, coinbase, and the EIP-2930 access list.
    warm_addresses: WarmAddresses,
    /// Persistent storage writes plus warm slot metadata for the current transaction.
    storage: AddressMap<StorageOverlay>,
    /// Revert journal.
    journal: Vec<JournalEntry>,
    /// Logs emitted by the current transaction.
    logs: Vec<Log>,
    /// Accounts self-destructed in the current transaction.
    selfdestructs: AddressSet,
    /// Transaction-scoped EIP-1153 transient storage keyed by account address and slot.
    transient_storage: StorageKeyMap<Word>,
}

impl State {
    /// Creates a new state over an initial database.
    pub fn new(initial: impl DynDatabase) -> Self {
        Self::new_mono(Box::new(initial))
    }

    pub(crate) fn new_mono(initial: Box<dyn DynDatabase>) -> Self {
        Self {
            database: CacheDB::new(initial),
            accounts: TrackedAccountMap::default(),
            warm_addresses: WarmAddresses::new(),
            storage: AddressMap::default(),
            journal: Vec::new(),
            logs: Vec::new(),
            selfdestructs: AddressSet::default(),
            transient_storage: StorageKeyMap::default(),
        }
    }

    /// Returns a checkpoint for later rollback.
    #[inline]
    pub const fn checkpoint(&self) -> StateCheckpoint {
        StateCheckpoint { journal_len: self.journal.len(), logs_len: self.logs.len() }
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
    pub fn storage_ref(&self, address: &Address, key: &Word) -> Option<Word> {
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
    /// database; use [`Self::account_info`] or [`Self::find`] when database-backed loading is
    /// desired.
    #[inline]
    #[must_use]
    pub fn account_ref(&self, address: &Address) -> Option<&Account> {
        self.accounts.get(address)?.present.as_ref()
    }

    /// Returns whether an account is warm in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_account_warm(&self, address: &Address) -> bool {
        self.accounts.is_warm(address) || self.warm_addresses.is_warm(address)
    }

    /// Returns the transaction-initial base warm set (precompiles, coinbase, access list).
    #[inline]
    #[must_use]
    pub const fn warm_addresses(&self) -> &WarmAddresses {
        &self.warm_addresses
    }

    /// Marks the precompile addresses as warm for the current transaction.
    ///
    /// This populates the base warm set and survives [`Self::rollback`], like the other
    /// `*_non_revertible` warming. The set persists until overwritten or until
    /// [`Self::clear_transaction_state`].
    #[inline]
    pub fn warm_precompiles(&mut self, addresses: &AddressSet) {
        self.warm_addresses.set_precompile_addresses(addresses);
    }

    /// Marks the coinbase/beneficiary address as warm for the current transaction (EIP-3651).
    #[inline]
    pub const fn warm_coinbase(&mut self, address: Address) {
        self.warm_addresses.set_coinbase(address);
    }

    /// Installs the EIP-2930 access list into the base warm set.
    ///
    /// Each address becomes warm, and each of its slots becomes a warm storage slot. Replaces any
    /// previously installed access list.
    #[inline]
    pub fn warm_access_list(&mut self, access_list: AddressMap<HashSet<Word>>) {
        self.warm_addresses.set_access_list(access_list);
    }

    /// Returns whether an account is touched in the current transaction.
    #[inline]
    #[must_use]
    fn is_account_touched(&self, address: &Address) -> bool {
        self.accounts.is_touched(address)
    }

    /// Marks an account as warm in a revertible execution context.
    ///
    /// Returns whether the account was cold before this access. If this call newly warms the
    /// account, the warm-set change is journaled and will be undone by [`Self::rollback`]. Use this
    /// for warmth introduced while executing EVM code or any other scope whose effects may be
    /// reverted to a checkpoint.
    #[inline(never)]
    #[must_use]
    pub fn warm_account(&mut self, address: &Address) -> bool {
        if self.warm_addresses.is_warm(address) {
            return false;
        }
        if self.accounts.warm_account(*address) {
            self.journal.push(JournalEntry::AccountWarmed { address: *address });
            true
        } else {
            false
        }
    }

    /// Marks an account as warm outside all revertible execution contexts.
    ///
    /// This intentionally does **not** journal the warm-set change. It must only be used for
    /// transaction-initial warmth that is established before any checkpoint that might be rolled
    /// back, such as base transaction warm addresses, precompiles, access-list entries, or other
    /// pre-execution transaction setup. Warmth added by this method survives [`Self::rollback`] and
    /// is cleared only by [`Self::clear_transaction_state`].
    ///
    /// Do not call this from EVM execution, nested calls, precompile execution, or any other
    /// revertible scope. Use [`Self::warm_account`] there so failed frames correctly restore the
    /// EIP-2929 access set.
    pub fn warm_account_non_revertible(&mut self, address: &Address) {
        let _ = self.accounts.warm_account(*address);
    }

    /// Marks accounts as warm in a revertible execution context.
    ///
    /// See [`Self::warm_account`] for rollback semantics.
    pub fn warm_accounts(&mut self, addresses: impl IntoIterator<Item = Address>) {
        let addresses = addresses.into_iter();
        self.accounts.reserve(addresses.size_hint().0);
        for address in addresses {
            let _ = self.warm_account(&address);
        }
    }

    /// Marks accounts as warm outside all revertible execution contexts.
    ///
    /// See [`Self::warm_account_non_revertible`] for the required usage restrictions. In
    /// particular, these warm-set changes are not journaled and are not undone by rollback.
    pub fn warm_accounts_non_revertible(&mut self, addresses: impl IntoIterator<Item = Address>) {
        let addresses = addresses.into_iter();
        self.accounts.reserve(addresses.size_hint().0);
        for address in addresses {
            self.warm_account_non_revertible(&address);
        }
    }

    /// Returns whether a storage slot is warm in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_storage_warm(&self, address: &Address, key: &Word) -> bool {
        self.storage
            .get(address)
            .and_then(|storage| storage.slots.get(key))
            .is_some_and(|slot| slot.warm)
            || self.warm_addresses.is_storage_warm(address, key)
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
        if self.warm_addresses.is_storage_warm(address, key) {
            return false;
        }
        let slot = self.storage.entry(*address).or_default().slots.entry(*key).or_default();
        if slot.mark_warm() {
            self.journal.push(JournalEntry::StorageWarmed { address: *address, key: *key });
            true
        } else {
            false
        }
    }

    /// Marks a storage slot as warm outside all revertible execution contexts.
    ///
    /// Returns whether the slot was cold before this access. This intentionally does **not**
    /// journal the warm-set change. It must only be used for transaction-initial warmth that is
    /// established before any checkpoint that might be rolled back, such as access-list storage
    /// slots. Warmth added by this method survives [`Self::rollback`] and is cleared only by
    /// [`Self::clear_transaction_state`].
    ///
    /// Do not call this from EVM execution, nested calls, precompile execution, or any other
    /// revertible scope. Use [`Self::warm_storage`] there so failed frames correctly restore the
    /// EIP-2929 access set.
    #[must_use]
    pub fn warm_storage_non_revertible(&mut self, address: &Address, key: &Word) -> bool {
        let slot = self.storage.entry(*address).or_default().slots.entry(*key).or_default();
        slot.mark_warm()
    }

    /// Clears transaction-scoped substate.
    pub fn clear_transaction_state(&mut self) {
        self.accounts.clear();
        self.warm_addresses.clear_coinbase_and_access_list();
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
        database: &mut dyn DynDatabase,
        accounts: &'a mut TrackedAccountMap,
        journal: &mut Vec<JournalEntry>,
        address: &Address,
    ) -> DbResult<&'a mut TrackedAccount> {
        match accounts.entry(*address) {
            hash_map::Entry::Occupied(entry) => {
                let entry = entry.into_mut();
                if !entry.is_loaded {
                    let original = database.get_account(address)?;
                    let present = original.clone().map(Account::from_info);
                    entry.original = original;
                    entry.present = present;
                    entry.is_loaded = true;
                    journal.push(JournalEntry::AccountInserted { address: *address });
                }
                Ok(entry)
            }
            hash_map::Entry::Vacant(entry) => {
                let original = database.get_account(address)?;
                let present = original.clone().map(Account::from_info);
                journal.push(JournalEntry::AccountInserted { address: *address });
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
        let entry = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.accounts,
            &mut self.journal,
            address,
        )?;
        if entry.present.is_none() {
            self.journal.push(JournalEntry::AccountChange { address: *address, previous: None });
            entry.present = Some(Account { code_hash: KECCAK256_EMPTY, ..Account::default() });
        }
        Ok(entry.present.as_mut().expect("account is inserted above"))
    }

    /// Loads `address` into the transaction overlay and returns a journaled mutation handle.
    ///
    /// Unlike [`Self::account_info`], which reads the backing database without caching, this reads
    /// the account once and preserves it in the transaction overlay. The returned
    /// [`JournaledAccount`] records a revert snapshot on its first mutation, so any changes made
    /// through it are undone together by [`Self::rollback`]. The account is materialized as empty
    /// only when it is first mutated while absent. This mirrors revm's `JournaledAccount`.
    pub fn journaled_account(&mut self, address: &Address) -> DbResult<JournaledAccount<'_>> {
        let tracked = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.accounts,
            &mut self.journal,
            address,
        )?;
        Ok(JournaledAccount::new(
            *address,
            tracked,
            &mut self.journal,
            &mut self.database,
            &mut self.warm_addresses,
        ))
    }

    fn journal_account_change(&mut self, address: &Address) -> DbResult<&mut Account> {
        Ok(self.journaled_account(address)?.into_account_mut())
    }

    /// Returns account info.
    #[inline(never)]
    pub fn account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        if let Some(present) =
            self.accounts.get(address).and_then(TrackedAccount::present_if_loaded)
        {
            return Ok(present.as_ref().map(Account::info));
        }
        self.database.get_account(address)
    }

    /// Returns whether an account is empty/non-existent for EIP-150 new-account gas checks.
    pub(super) fn target_is_empty_for_new_account_gas(
        &mut self,
        address: &Address,
        features: EvmFeatures,
    ) -> DbResult<bool> {
        if features.contains(EvmFeatures::EIP161) {
            return Ok(self.account_info(address)?.is_none_or(|info| info.is_empty()));
        }
        Ok(self.account_info(address)?.is_none() && !self.is_account_touched(address))
    }

    /// Returns an account if it exists.
    pub fn find(&mut self, address: &Address) -> DbResult<Option<&Account>> {
        let entry = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.accounts,
            &mut self.journal,
            address,
        )?;
        Ok(entry.present.as_ref())
    }

    /// Gets account code.
    pub fn get_code(&mut self, address: &Address) -> DbResult<Bytecode> {
        if let Some(account) = self.accounts.get(address).and_then(|entry| entry.present.as_ref()) {
            if account.code_hash == KECCAK256_EMPTY {
                return Ok(Bytecode::default());
            }
            if !account.code.is_empty() {
                return Ok(account.code.clone());
            }
            let code_hash = account.code_hash;
            return self.database.get_code_by_hash(&code_hash);
        }

        let Some(info) = self.database.get_account(address)? else {
            return Ok(Bytecode::default());
        };
        if info.code_hash == KECCAK256_EMPTY {
            return Ok(Bytecode::default());
        }
        if let Some(code) = info.code
            && !code.is_empty()
        {
            return Ok(code);
        }
        self.database.get_code_by_hash(&info.code_hash)
    }

    fn current_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        if let Some(storage) = self.storage.get(address) {
            if let Some(slot) = storage.slots.get(key).and_then(|slot| slot.value.as_ref()) {
                return Ok(slot.current);
            }
            if storage.wiped {
                return Ok(Word::ZERO);
            }
        }
        if self.database.account_absent(address) {
            return Ok(Word::ZERO);
        }
        self.database.get_storage(address, key)
    }

    fn cache_storage_value(&mut self, address: &Address, key: &Word, value: Word) {
        let slot = self.storage.entry(*address).or_default().slots.entry(*key).or_default();
        slot.value.get_or_insert(Tracked::new(value));
    }

    fn insert_transaction_storage(
        &mut self,
        address: &Address,
        key: &Word,
        original: Word,
        value: Word,
    ) {
        let storage = self.storage.entry(*address).or_default();
        match storage.slots.entry(*key) {
            hash_map::Entry::Occupied(mut entry) => {
                let slot = entry.get_mut();
                match &mut slot.value {
                    Some(tracked) => {
                        let previous = tracked.current;
                        if previous != value {
                            tracked.current = value;
                            self.journal.push(JournalEntry::StorageChange {
                                address: *address,
                                key: *key,
                                previous,
                            });
                        }
                    }
                    None => {
                        slot.value =
                            Some(Tracked { original, current: value, _non_exhaustive: () });
                        self.journal
                            .push(JournalEntry::StorageInserted { address: *address, key: *key });
                    }
                }
            }
            hash_map::Entry::Vacant(entry) => {
                entry.insert(StorageSlot {
                    value: Some(Tracked { original, current: value, _non_exhaustive: () }),
                    warm: false,
                    _non_exhaustive: (),
                });
                self.journal.push(JournalEntry::StorageInserted { address: *address, key: *key });
            }
        }
    }

    /// Loads persistent storage.
    pub fn storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let Some(_) = self.account_info(address)? else {
            return Ok(Word::ZERO);
        };
        let value = self.current_storage(address, key)?;
        self.cache_storage_value(address, key, value);
        Ok(value)
    }

    /// Stores persistent storage and returns values needed for `SSTORE` gas metering.
    ///
    /// This is a raw state mutation helper, not the full EVM `SSTORE` host operation. It does
    /// not perform static-call checks, gas/stipend checks, EIP-2929 cold-access handling, refund
    /// accounting, or Amsterdam state-gas charging. Instruction implementations should call the
    /// host `sstore` operation instead, and only use this lower-level helper when those concerns
    /// are handled elsewhere.
    pub fn set_storage(&mut self, address: &Address, key: &Word, value: &Word) -> DbResult<SStore> {
        let _ = self.get_or_insert(address)?;
        self.touch(address);
        let storage = self.storage.get(address);
        let original_value =
            if storage.is_some_and(|s| s.wiped) || self.database.account_absent(address) {
                Word::ZERO
            } else {
                self.database.get_storage(address, key)?
            };
        let present_value = storage
            .and_then(|storage| storage.slots.get(key))
            .and_then(|slot| slot.value.as_ref())
            .map_or(original_value, |slot| slot.current);
        let result = SStore {
            original_value,
            present_value,
            new_value: *value,
            is_cold: false,
            _non_exhaustive: (),
        };
        if present_value != *value {
            self.insert_transaction_storage(address, key, original_value, *value);
        }
        Ok(result)
    }

    /// Marks an account as touched by the current transaction.
    pub fn touch(&mut self, address: &Address) {
        if self.accounts.touch(*address) {
            self.journal.push(JournalEntry::Touch { address: *address });
        }
    }

    /// Adds a signed balance delta by wrapping two's-complement values.
    pub fn add_balance(&mut self, address: &Address, delta: &Word) -> DbResult<()> {
        if delta.is_zero() {
            self.touch(address);
            return Ok(());
        }
        let account = self.journal_account_change(address)?;
        account.balance = account.balance.wrapping_add(*delta);
        self.touch(address);
        Ok(())
    }

    /// Transfers value between accounts.
    pub fn transfer(&mut self, from: &Address, to: &Address, value: &Word) -> DbResult<bool> {
        if value.is_zero() {
            self.touch(to);
            return Ok(true);
        }

        let from_balance = self.account_info(from)?.map_or(Word::ZERO, |info| info.balance);
        if from == to {
            if from_balance < *value {
                return Ok(false);
            }
            self.touch(to);
            return Ok(true);
        }
        let Some(new_from_balance) = from_balance.checked_sub(*value) else {
            return Ok(false);
        };

        self.journal_account_change(from)?.balance = new_from_balance;
        self.touch(from);

        let account = self.journal_account_change(to)?;
        account.balance = account.balance.saturating_add(*value);
        self.touch(to);
        Ok(true)
    }

    /// Increments account nonce.
    #[inline(never)]
    pub fn increment_nonce(&mut self, address: &Address) -> DbResult<()> {
        let account = self.journal_account_change(address)?;
        account.nonce = account.nonce.saturating_add(1);
        self.touch(address);
        Ok(())
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
        if let Some(info) = self.account_info(address)?
            && (info.nonce != 0 || info.code_hash != KECCAK256_EMPTY)
        {
            return Ok(Err(InstrStop::CreateCollision));
        }

        if !self.transfer(caller, address, value)? {
            return Ok(Err(InstrStop::OutOfFunds));
        }

        let balance = self.get_or_insert(address)?.balance;
        self.wipe_storage(address);
        let account = self.journal_account_change(address)?;
        *account = Account {
            nonce: u64::from(features.contains(EvmFeatures::EIP161)),
            balance,
            code_hash: KECCAK256_EMPTY,
            code: Bytecode::default(),
            just_created: true,
            code_changed: true,
            _non_exhaustive: (),
        };
        self.touch(address);
        Ok(Ok(()))
    }

    /// Sets account bytecode.
    pub fn set_code(&mut self, address: &Address, code: Bytecode) -> DbResult<()> {
        let account = self.journal_account_change(address)?;
        account.code_hash = code.hash_slow();
        account.code = code;
        account.code_changed = true;
        Ok(())
    }

    /// Marks all prior persistent storage for `address` as deleted.
    pub fn wipe_storage(&mut self, address: &Address) {
        let previous = self.storage.get(address).cloned();
        let mut wiped =
            StorageOverlay { wiped: true, slots: U256Map::default(), _non_exhaustive: () };
        if let Some(previous) = &previous {
            for (&key, slot) in &previous.slots {
                if slot.warm {
                    wiped
                        .slots
                        .insert(key, StorageSlot { value: None, warm: true, _non_exhaustive: () });
                }
            }
        }
        self.storage.insert(*address, wiped);
        self.journal.push(JournalEntry::StorageWipe { address: *address, previous });
    }

    /// Loads transient storage.
    #[must_use]
    pub fn transient_storage(&mut self, address: &Address, key: &Word) -> Word {
        self.transient_storage.get(&StorageKey::new(*address, *key)).copied().unwrap_or_default()
    }

    /// Stores transient storage.
    pub fn set_transient_storage(&mut self, address: &Address, key: &Word, value: &Word) {
        match self.transient_storage.entry(StorageKey::new(*address, *key)) {
            hash_map::Entry::Occupied(mut entry) => {
                let previous = *entry.get();
                if previous == *value {
                    return;
                }
                self.journal.push(JournalEntry::TransientStorageChange {
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
                self.journal.push(JournalEntry::TransientStorageChange {
                    address: *address,
                    key: *key,
                    previous: None,
                });
                entry.insert(*value);
            }
        }
    }

    /// Marks an account as self-destructed in the current transaction.
    pub fn mark_destructed(&mut self, address: &Address) {
        let _ = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.accounts,
            &mut self.journal,
            address,
        );
        if self.selfdestructs.insert(*address) {
            self.journal.push(JournalEntry::SelfDestruct { address: *address });
        }
        self.touch(address);
    }

    /// Returns whether an account has been marked self-destructed in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_selfdestructed(&self, address: &Address) -> bool {
        self.selfdestructs.contains(address)
    }

    /// Returns whether an account was created in the current transaction.
    #[inline]
    #[must_use]
    pub(super) fn is_created_in_transaction(&self, address: &Address) -> bool {
        self.account_ref(address).is_some_and(|account| account.just_created)
    }

    fn cleanup_account_entry(&mut self, address: &Address) {
        if self.accounts.get(address).is_some_and(TrackedAccount::is_empty) {
            self.accounts.remove(address);
        }
    }

    fn cleanup_storage_slot(&mut self, address: &Address, key: &Word) {
        let remove_storage = if let Some(storage) = self.storage.get_mut(address) {
            if storage.slots.get(key).is_some_and(StorageSlot::is_empty) {
                storage.slots.remove(key);
            }
            !storage.wiped && storage.slots.is_empty()
        } else {
            false
        };
        if remove_storage {
            self.storage.remove(address);
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
                    if let Some(entry) = self.accounts.get_mut(&address) {
                        entry.is_loaded = false;
                        entry.original = None;
                        entry.present = None;
                    }
                    self.cleanup_account_entry(&address);
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
                    self.cleanup_account_entry(&address);
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
                    if let Some(storage) = self.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                    {
                        slot.value = None;
                    }
                    self.cleanup_storage_slot(&address, &key);
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
                    if let Some(entry) = self.accounts.get_mut(&address) {
                        entry.is_warm = false;
                    }
                    self.cleanup_account_entry(&address);
                }
                JournalEntry::StorageWarmed { address, key } => {
                    if let Some(storage) = self.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                    {
                        slot.warm = false;
                    }
                    self.cleanup_storage_slot(&address, &key);
                }
            }
        }
    }

    /// Returns whether an existing account is dead by the EIP-161 definition.
    ///
    /// Accounts with zero nonce, zero balance, and empty code are dead. Starting
    /// in Spurious Dragon, touched dead accounts that exist in the pre/final
    /// overlay state are deleted during transaction finalization. Non-existent
    /// touched accounts stay non-existent.
    fn is_existing_dead(&mut self, address: &Address) -> DbResult<bool> {
        if let Some(entry) = self.accounts.get(address)
            && entry.is_loaded
        {
            return Ok(entry.present.as_ref().is_some_and(Account::is_empty)
                || (entry.present.is_none() && entry.original.is_some()));
        }
        Ok(self.load_account(address)?.as_ref().is_some_and(Account::is_empty))
    }

    fn account_exists(&mut self, address: &Address) -> DbResult<bool> {
        if let Some(present) =
            self.accounts.get(address).and_then(TrackedAccount::present_if_loaded)
        {
            return Ok(present.is_some());
        }
        Ok(self.load_account(address)?.is_some())
    }

    fn delete_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        let entry = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.accounts,
            &mut self.journal,
            address,
        )?;
        let previous = entry.present.clone();
        self.journal.push(JournalEntry::AccountChange { address: *address, previous });
        entry.present = None;
        self.wipe_storage(address);
        Ok(())
    }

    fn materialize_empty_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        let original_exists = self.load_account(address)?.is_some();
        let entry = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.accounts,
            &mut self.journal,
            address,
        )?;
        if !original_exists && entry.present.is_none() {
            self.journal.push(JournalEntry::AccountChange { address: *address, previous: None });
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
                if self.is_existing_dead(address)? {
                    self.delete_account_for_finalization(address)?;
                }
            }
        } else {
            for address in &touched {
                // Before EIP-161, touching a non-existent account materializes it as empty.
                if !selfdestructs.contains(address) && !self.account_exists(address)? {
                    self.materialize_empty_account_for_finalization(address)?;
                }
            }
        }

        self.selfdestructs = selfdestructs;
        self.selfdestructs.clear();

        for address in touched {
            if let Some(entry) = self.accounts.get_mut(&address) {
                entry.is_touched = false;
            }
            self.cleanup_account_entry(&address);
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
