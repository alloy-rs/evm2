//! Basic in-memory EVM host state.

mod account;
mod block;
mod changes;
mod journal;
mod stream;

pub use account::{Account, AccountInfo, StorageOverlay, Tracked};
pub use block::BlockStateAccumulator;
pub use changes::{AccountChange, StateChanges};
pub use journal::{JournalEntry, StateCheckpoint};
pub use stream::{
    AccountChangeRef, AccountInfoRef, NoopChangeSink, StateChangeSink, StateChangeSource,
    StorageChange, Tee,
};

use crate::{
    EvmFeatures, Version,
    bytecode::Bytecode,
    evm::{
        SStore,
        db::{CacheDB, DbResult, DynDatabase},
        eip7708_burn_log,
    },
    interpreter::{InstrStop, Word},
    storage_key::{StorageKey, StorageKeyMap, StorageKeySet},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY, Log,
    map::{AddressMap, AddressSet, U256Map, hash_map},
};
use core::mem;
use derive_where::derive_where;

/// Reusable transaction-local state.
#[derive(Debug, Default)]
struct TxScratch {
    /// Account writes for the current transaction.
    accounts: AddressMap<Option<Account>>,
    /// Persistent storage writes for the current transaction.
    storage: AddressMap<StorageOverlay>,
    /// Revert journal.
    journal: Vec<JournalEntry>,
    /// Logs emitted by the current transaction.
    logs: Vec<Log>,
    /// Accounts touched for transaction-finalization account-lifetime rules.
    ///
    /// This is separate from the account overlay and the EIP-2929 warm set. A touched account may
    /// have no field changes, but can still matter for empty account deletion/materialization
    /// rules across forks.
    touched: AddressSet,
    /// Accounts self-destructed in the current transaction.
    selfdestructs: AddressSet,
    /// Self-destructed accounts that were also created in the current transaction.
    created_selfdestructs: AddressSet,
    /// Transaction-scoped warm account set for EIP-2929 gas accounting.
    ///
    /// This tracks whether account access is warm or cold. It does not imply the account was
    /// touched, changed, or should be emitted in [`StateChanges`].
    accessed_accounts: AddressSet,
    /// Transaction-scoped warm storage slot set.
    accessed_storage: StorageKeySet,
    /// Transaction-scoped EIP-1153 transient storage keyed by account address and slot.
    transient_storage: StorageKeyMap<Word>,
}

impl TxScratch {
    /// Clears transaction-scoped substate while retaining allocated buffers.
    fn clear_transaction_state(&mut self) {
        self.accounts.clear();
        self.storage.clear();
        self.journal.clear();
        self.touched.clear();
        self.selfdestructs.clear();
        self.created_selfdestructs.clear();
        self.accessed_accounts.clear();
        self.accessed_storage.clear();
        self.transient_storage.clear();
        self.logs.clear();
    }
}

/// Mutable EVM state with an accepted-state cache, transaction scratch, and reversible journal.
///
/// The state has three read layers:
///
/// - The initial database is the immutable backing state supplied by the caller.
/// - The accepted overlay caches database reads and stores changes committed at transaction
///   boundaries.
/// - The transaction scratch stores the in-flight transaction overlay, including journaled account
///   loads, slot loads, writes, deletes, warm accesses, logs, and transient storage.
///
/// Method names describe how a read interacts with those layers:
///
/// - `get_*` methods take `&self` and only inspect already-known in-memory state. They never load
///   missing accounts, code, or storage from the backing database.
/// - `read_*` methods take `&mut self` and may read through the accepted overlay and backing
///   database, but do not record a transaction load. They may populate the accepted overlay cache.
/// - `load_*` methods take `&mut self`, read the EVM-visible value, and record the account or slot
///   in transaction scratch so it participates in transaction state changes.
///
/// Mapping from revm host and journal method names:
///
/// - `basic` / `basic_ref` correspond to [`DynDatabase::get_account`]. Use
///   [`Self::read_account_info`] for a non-recording state read, or [`Self::load_account_info`] /
///   [`Self::load_account`] for a transaction-recorded load.
/// - `code_by_hash` / `code_by_hash_ref` correspond to [`DynDatabase::get_code_by_hash`]. Use
///   [`Self::read_code`] when resolving account code by address.
/// - `storage` / `storage_ref` on the database correspond to [`DynDatabase::get_storage`]. Use
///   [`Self::read_committed_storage`] for a committed-state read, [`Self::read_storage`] for the
///   current transaction-visible value, or [`Self::load_storage`] for a transaction-recorded load.
/// - `load_account` corresponds to [`Self::load_account`] when the caller needs the loaded account
///   object, or [`Self::load_account_info`] when only the account info is needed.
/// - `sload` corresponds to [`Self::load_storage`].
/// - Map-style state lookups correspond to [`Self::get_account`], and already-loaded slot lookups
///   correspond to [`Self::get_storage_slot`].
#[derive_where(Debug)]
#[non_exhaustive]
pub struct State {
    /// Database plus accepted transaction-boundary state overlay.
    #[derive_where(skip)]
    database: CacheDB<Box<dyn DynDatabase>>,
    /// Reusable transaction-local state.
    scratch: TxScratch,
}

impl State {
    /// Creates a new state over an initial database.
    pub fn new(initial: impl DynDatabase) -> Self {
        Self::new_mono(Box::new(initial))
    }

    pub(crate) fn new_mono(initial: Box<dyn DynDatabase>) -> Self {
        Self { database: CacheDB::new(initial), scratch: TxScratch::default() }
    }

    /// Returns a checkpoint for later rollback.
    #[inline]
    pub const fn checkpoint(&self) -> StateCheckpoint {
        StateCheckpoint {
            journal_len: self.scratch.journal.len(),
            logs_len: self.scratch.logs.len(),
        }
    }

    /// Returns the accepted-state overlay database.
    #[inline]
    pub fn overlay_db(&self) -> &CacheDB<Box<dyn DynDatabase>> {
        &self.database
    }

    /// Returns the accepted-state overlay database mutably.
    #[inline]
    pub fn overlay_db_mut(&mut self) -> &mut CacheDB<Box<dyn DynDatabase>> {
        &mut self.database
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

    /// Applies borrowed changes to the accepted state overlay.
    #[inline]
    pub fn commit_source<S: StateChangeSource>(&mut self, source: &S) {
        self.database.commit_source(source);
    }

    /// Loads a historical block hash.
    #[inline]
    pub(crate) fn block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        self.database.get_block_hash(number)
    }

    /// Returns logs emitted by the current in-flight transaction.
    #[inline]
    pub fn logs(&self) -> &[Log] {
        &self.scratch.logs
    }

    /// Takes logs emitted by the current in-flight transaction.
    #[inline]
    pub(crate) fn take_logs(&mut self) -> Vec<Log> {
        mem::take(&mut self.scratch.logs)
    }

    /// Returns the reversible journal entries for the current transaction.
    #[inline]
    pub fn journal(&self) -> &[JournalEntry] {
        &self.scratch.journal
    }

    /// Records a transaction log.
    #[inline]
    pub fn log(&mut self, log: Log) {
        self.scratch.logs.push(log);
    }

    /// Gets a storage value only if it is already known in memory.
    ///
    /// This is an `&self` lookup over the current transaction storage overlay and the accepted
    /// overlay cache. It also returns zero for accounts known to be absent. It never loads an
    /// account or slot from the backing database, so callers must handle `None` when the value is
    /// not cached. Use [`Self::read_storage`] when a database-backed read is desired.
    #[must_use]
    pub fn get_storage(&self, address: &Address, key: &Word) -> Option<Word> {
        if let Some(storage) = self.scratch.storage.get(address) {
            if let Some(slot) = storage.slots.get(key) {
                return Some(slot.current);
            }
            if storage.wiped {
                return Some(Word::ZERO);
            }
        }
        if self.account_known_absent(address) {
            return Some(Word::ZERO);
        }
        self.database.storage_ref(address, key)
    }

    /// Gets a storage slot from the current transaction overlay.
    ///
    /// The returned value includes both the original and current slot values for this transaction.
    /// This is an `&self` lookup and only returns slots that have already been loaded or written in
    /// the transaction scratch. It does not inspect the accepted overlay or backing database.
    #[must_use]
    pub fn get_storage_slot(&self, address: &Address, key: &Word) -> Option<&Tracked<Word>> {
        self.scratch.storage.get(address)?.slots.get(key)
    }

    /// Gets an account from the current transaction overlay if it is present and alive.
    ///
    /// This is an `&self` lookup into transaction scratch only. It returns `None` both when the
    /// account has not been loaded in this transaction and when the transaction overlay marks the
    /// account as deleted. Use [`Self::read_account_info`] for a database-backed read.
    #[must_use]
    pub fn get_account(&self, address: &Address) -> Option<&Account> {
        self.scratch.accounts.get(address)?.as_ref()
    }

    /// Reads account info without recording an account load in transaction state.
    ///
    /// This observes the current transaction account overlay first, including deleted accounts,
    /// then reads through the accepted overlay and backing database. It may populate the accepted
    /// overlay cache, but it does not insert the account into transaction scratch and therefore
    /// does not make an unchanged account appear in [`StateChanges`].
    #[inline(never)]
    pub fn read_account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        if let Some(account) = self.scratch.accounts.get(address) {
            return Ok(account.as_ref().map(Account::info));
        }
        self.database.get_account(address)
    }

    /// Loads account info and records the account in transaction state.
    ///
    /// This is the EVM-semantic account load: the loaded account becomes part of the transaction
    /// state and is emitted in [`StateChanges`] even if it is never changed. Use
    /// [`Self::read_account_info`] for reads that must not be recorded.
    #[inline(never)]
    pub fn load_account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        Ok(account.as_ref().map(Account::info))
    }

    /// Reads account code without recording an account load in transaction state.
    ///
    /// This observes the current transaction account overlay first, including deleted accounts,
    /// then reads account/code data through the accepted overlay and backing database. Missing
    /// bytecode is resolved by hash. The read may populate the accepted overlay cache, but it does
    /// not insert the account into transaction scratch.
    pub fn read_code(&mut self, address: &Address) -> DbResult<Bytecode> {
        let Some(info) = self.read_account_info(address)? else {
            return Ok(Bytecode::default());
        };
        self.code_from_info(info)
    }

    fn code_from_info(&mut self, info: AccountInfo) -> DbResult<Bytecode> {
        if let Some(code) = info.code
            && !code.is_empty()
        {
            return Ok(code);
        }
        self.code_from_parts(info.code_hash, Bytecode::default())
    }

    fn code_from_parts(&mut self, code_hash: B256, code: Bytecode) -> DbResult<Bytecode> {
        if code_hash == KECCAK256_EMPTY {
            return Ok(Bytecode::default());
        }
        if !code.is_empty() {
            return Ok(code);
        }
        self.database.get_code_by_hash(&code_hash)
    }

    /// Reads storage from committed state, ignoring the current transaction overlay.
    ///
    /// This reads through the accepted overlay and backing database only. It intentionally ignores
    /// storage writes, storage wipes, and loaded slots in transaction scratch. This is useful for
    /// callers that need the pre-transaction or committed view rather than the current EVM view.
    pub fn read_committed_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.database.get_storage(address, key)
    }

    /// Returns whether an account is empty/non-existent for EIP-150 new-account gas checks.
    pub(crate) fn target_is_empty_for_new_account_gas(
        &mut self,
        address: &Address,
        features: EvmFeatures,
    ) -> DbResult<bool> {
        if features.contains(EvmFeatures::EIP161) {
            return Ok(self.read_account_info(address)?.is_none_or(|info| info.is_empty()));
        }
        Ok(self.read_account_info(address)?.is_none() && !self.scratch.touched.contains(address))
    }

    /// Loads an account into transaction state if it exists.
    ///
    /// This records the account in transaction scratch and journals the insertion so rollback can
    /// remove it again. Loading an unchanged account makes it eligible to be emitted in
    /// [`StateChanges`], matching EVM account-load semantics.
    pub fn load_account(&mut self, address: &Address) -> DbResult<Option<&Account>> {
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        Ok(account.as_ref())
    }

    /// Reads storage visible to the current transaction without recording a slot load.
    ///
    /// This observes transaction storage writes and wipes first, then reads through the accepted
    /// overlay and backing database. It does not insert the slot into transaction scratch, so an
    /// unchanged slot read this way will not be emitted in [`StateChanges`]. Use
    /// [`Self::load_storage`] for EVM `SLOAD` semantics.
    pub fn read_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        if let Some(storage) = self.scratch.storage.get(address) {
            if let Some(slot) = storage.slots.get(key) {
                return Ok(slot.current);
            }
            if storage.wiped {
                return Ok(Word::ZERO);
            }
        }
        if self.account_known_absent(address) {
            return Ok(Word::ZERO);
        }
        self.database.get_storage(address, key)
    }

    /// Loads storage and records the slot in transaction state.
    ///
    /// This is the EVM-semantic storage load: the loaded slot becomes part of the transaction
    /// state and is emitted in [`StateChanges`] even if it is never written. The recorded slot is
    /// intentionally not journaled so that it survives rollback. Use [`Self::read_storage`] for
    /// current-state reads that must not be recorded.
    pub fn load_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let value = self.read_storage(address, key)?;
        self.scratch
            .storage
            .entry(*address)
            .or_default()
            .slots
            .entry(*key)
            .or_insert_with(|| Tracked::new(value));
        Ok(value)
    }

    /// Returns whether an account is warm in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_account_warm(&self, address: &Address) -> bool {
        self.scratch.accessed_accounts.contains(address)
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
        if self.scratch.accessed_accounts.insert(*address) {
            self.scratch.journal.push(JournalEntry::AccountWarmed { address: *address });
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
        self.scratch.accessed_accounts.insert(*address);
    }

    /// Marks accounts as warm in a revertible execution context.
    ///
    /// See [`Self::warm_account`] for rollback semantics.
    pub fn warm_accounts(&mut self, addresses: impl IntoIterator<Item = Address>) {
        let addresses = addresses.into_iter();
        self.scratch.accessed_accounts.reserve(addresses.size_hint().0);
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
        self.scratch.accessed_accounts.reserve(addresses.size_hint().0);
        for address in addresses {
            self.warm_account_non_revertible(&address);
        }
    }

    /// Returns whether a storage slot is warm in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_storage_warm(&self, address: &Address, key: &Word) -> bool {
        self.scratch.accessed_storage.contains(&StorageKey::new(*address, *key))
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
        if self.scratch.accessed_storage.insert(StorageKey::new(*address, *key)) {
            self.scratch.journal.push(JournalEntry::StorageWarmed { address: *address, key: *key });
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
        self.scratch.accessed_storage.insert(StorageKey::new(*address, *key))
    }

    /// Clears transaction-scoped substate.
    pub fn clear_transaction_state(&mut self) {
        self.scratch.clear_transaction_state();
    }

    fn get_db_account(&mut self, address: &Address) -> DbResult<Option<Account>> {
        Ok(self.database.get_account(address)?.map(Account::from_info))
    }

    fn ensure_transaction_account<'a>(
        database: &mut dyn DynDatabase,
        accounts: &'a mut AddressMap<Option<Account>>,
        journal: &mut Vec<JournalEntry>,
        address: &Address,
    ) -> DbResult<&'a mut Option<Account>> {
        match accounts.entry(*address) {
            hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            hash_map::Entry::Vacant(entry) => {
                let original = database.get_account(address)?.map(Account::from_info);
                journal.push(JournalEntry::AccountInserted { address: *address });
                Ok(entry.insert(original))
            }
        }
    }

    /// Gets an existing account or inserts a new empty account.
    pub fn get_or_insert(&mut self, address: &Address) -> DbResult<&mut Account> {
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        Ok(account.get_or_insert_with(|| {
            self.scratch
                .journal
                .push(JournalEntry::AccountChange { address: *address, previous: None });
            Account { code_hash: KECCAK256_EMPTY, ..Account::default() }
        }))
    }

    fn journal_account_change(&mut self, address: &Address) -> DbResult<&mut Account> {
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        let previous = account.clone();
        self.scratch.journal.push(JournalEntry::AccountChange { address: *address, previous });
        Ok(account
            .get_or_insert_with(|| Account { code_hash: KECCAK256_EMPTY, ..Account::default() }))
    }

    #[inline]
    fn account_known_absent(&self, address: &Address) -> bool {
        self.scratch.accounts.get(address).is_some_and(Option::is_none)
            || self.database.account_absent(address)
    }

    fn insert_transaction_storage(
        &mut self,
        address: &Address,
        key: &Word,
        original: Word,
        value: Word,
    ) {
        let storage = self.scratch.storage.entry(*address).or_default();
        match storage.slots.entry(*key) {
            hash_map::Entry::Occupied(mut entry) => {
                let previous = entry.get().current;
                if previous != value {
                    entry.get_mut().set_current(value);
                    self.scratch.journal.push(JournalEntry::StorageChange {
                        address: *address,
                        key: *key,
                        previous,
                    });
                }
            }
            hash_map::Entry::Vacant(entry) => {
                entry.insert(Tracked::from_parts(original, value));
                self.scratch
                    .journal
                    .push(JournalEntry::StorageInserted { address: *address, key: *key });
            }
        }
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
        let storage = self.scratch.storage.get(address);
        let original_value =
            if storage.is_some_and(|s| s.wiped) || self.database.account_absent(address) {
                Word::ZERO
            } else {
                self.database.get_storage(address, key)?
            };
        let present_value = storage
            .and_then(|storage| storage.slots.get(key))
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
        if self.scratch.touched.insert(*address) {
            self.scratch.journal.push(JournalEntry::Touch { address: *address });
        }
    }

    /// Adds a signed balance delta by wrapping two's-complement values.
    pub fn add_balance(&mut self, address: &Address, delta: &Word) -> DbResult<()> {
        if delta.is_zero() {
            let _ = self.load_account_info(address)?;
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
            let _ = self.load_account_info(to)?;
            self.touch(to);
            return Ok(true);
        }

        let from_balance = self.load_account_info(from)?.map_or(Word::ZERO, |info| info.balance);
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
        if let Some(info) = self.read_account_info(address)?
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
        let previous = self.scratch.storage.insert(
            *address,
            StorageOverlay { wiped: true, slots: U256Map::default(), _non_exhaustive: () },
        );
        self.scratch.journal.push(JournalEntry::StorageWipe { address: *address, previous });
    }

    /// Loads transient storage.
    #[must_use]
    pub fn transient_storage(&mut self, address: &Address, key: &Word) -> Word {
        self.scratch
            .transient_storage
            .get(&StorageKey::new(*address, *key))
            .copied()
            .unwrap_or_default()
    }

    /// Stores transient storage.
    pub fn set_transient_storage(&mut self, address: &Address, key: &Word, value: &Word) {
        match self.scratch.transient_storage.entry(StorageKey::new(*address, *key)) {
            hash_map::Entry::Occupied(mut entry) => {
                let previous = *entry.get();
                if previous == *value {
                    return;
                }
                self.scratch.journal.push(JournalEntry::TransientStorageChange {
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
                self.scratch.journal.push(JournalEntry::TransientStorageChange {
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
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        );
        if self.scratch.selfdestructs.insert(*address) {
            self.scratch.journal.push(JournalEntry::SelfDestruct { address: *address });
        }
        self.touch(address);
    }

    /// Returns whether an account has been marked self-destructed in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_selfdestructed(&self, address: &Address) -> bool {
        self.scratch.selfdestructs.contains(address)
    }

    /// Returns whether an account was created in the current transaction.
    #[inline]
    #[must_use]
    pub(crate) fn is_created_in_transaction(&self, address: &Address) -> bool {
        self.get_account(address).is_some_and(|account| account.just_created)
    }

    /// Reverts state changes after the checkpoint.
    #[inline(never)]
    pub fn rollback(&mut self, checkpoint: StateCheckpoint, features: EvmFeatures) {
        assert!(
            checkpoint.journal_len <= self.scratch.journal.len(),
            "checkpoint is past journal length"
        );
        assert!(checkpoint.logs_len <= self.scratch.logs.len(), "checkpoint is past logs length");
        self.scratch.logs.truncate(checkpoint.logs_len);
        while self.scratch.journal.len() != checkpoint.journal_len {
            let Some(entry) = self.scratch.journal.pop() else {
                unreachable!("checkpoint is checked above")
            };
            match entry {
                JournalEntry::AccountChange { address, previous } => {
                    if let Some(account) = self.scratch.accounts.get_mut(&address) {
                        *account = previous;
                    }
                }
                JournalEntry::AccountInserted { address } => {
                    // Keep the account recorded as a load so that reverted frames still
                    // contribute loaded accounts to [`StateChanges`]. The original database value
                    // is cached by the insertion in [`Self::ensure_transaction_account`].
                    if let Some(account) = self.scratch.accounts.get_mut(&address) {
                        *account =
                            self.database.account_info(&address).cloned().map(Account::from_info);
                    }
                }
                JournalEntry::Touch { address } => {
                    // EIP-161 preserves the historical Yellow Paper K.1 precompile-3 touch.
                    if features.contains(EvmFeatures::EIP161)
                        && address == Address::with_last_byte(3)
                    {
                        continue;
                    }
                    self.scratch.touched.remove(&address);
                }
                JournalEntry::SelfDestruct { address } => {
                    self.scratch.selfdestructs.remove(&address);
                }
                JournalEntry::StorageChange { address, key, previous } => {
                    if let Some(storage) = self.scratch.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                    {
                        slot.set_current(previous);
                    }
                }
                JournalEntry::StorageInserted { address, key } => {
                    // Keep the slot recorded as a load so that reverted frames still contribute
                    // loaded slots to [`StateChanges`].
                    if let Some(storage) = self.scratch.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                    {
                        slot.set_current(slot.original);
                    }
                }
                JournalEntry::StorageWipe { address, previous } => match previous {
                    Some(storage) => {
                        self.scratch.storage.insert(address, storage);
                    }
                    None => {
                        self.scratch.storage.remove(&address);
                    }
                },
                JournalEntry::TransientStorageChange { address, key, previous } => match previous {
                    Some(previous) if !previous.is_zero() => {
                        self.scratch
                            .transient_storage
                            .insert(StorageKey::new(address, key), previous);
                    }
                    _ => {
                        self.scratch.transient_storage.remove(&StorageKey::new(address, key));
                    }
                },
                JournalEntry::AccountWarmed { address } => {
                    self.scratch.accessed_accounts.remove(&address);
                }
                JournalEntry::StorageWarmed { address, key } => {
                    self.scratch.accessed_storage.remove(&StorageKey::new(address, key));
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
        if let Some(account) = self.scratch.accounts.get(address) {
            return Ok(account.as_ref().is_some_and(Account::is_empty)
                || (account.is_none() && self.database.account_info(address).is_some()));
        }
        Ok(self.get_db_account(address)?.as_ref().is_some_and(Account::is_empty))
    }

    fn account_exists(&mut self, address: &Address) -> DbResult<bool> {
        if let Some(account) = self.scratch.accounts.get(address) {
            return Ok(account.is_some());
        }
        Ok(self.get_db_account(address)?.is_some())
    }

    fn delete_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        let previous = account.clone();
        self.scratch.journal.push(JournalEntry::AccountChange { address: *address, previous });
        *account = None;
        self.wipe_storage(address);
        Ok(())
    }

    fn materialize_empty_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        let original_exists = self.get_db_account(address)?.is_some();
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        if !original_exists {
            account.get_or_insert_with(|| {
                self.scratch
                    .journal
                    .push(JournalEntry::AccountChange { address: *address, previous: None });
                Account { code_hash: KECCAK256_EMPTY, ..Account::default() }
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn finalize_transaction_(&mut self, version: &Version) {
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
    pub(crate) fn finalize_transaction(
        &mut self,
        version: &Version,
        mut inspect_log: impl FnMut(&Log),
    ) -> DbResult<()> {
        let selfdestructs = mem::take(&mut self.scratch.selfdestructs);
        let touched = mem::take(&mut self.scratch.touched);

        let delayed_burn_logs =
            version.feature(EvmFeatures::EIP7708 | EvmFeatures::EIP7708_DELAYED_BURN);
        if delayed_burn_logs {
            let mut burned = Vec::new();
            for &address in &selfdestructs {
                if let Some(balance) = self
                    .scratch
                    .accounts
                    .get(&address)
                    .and_then(Option::as_ref)
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
            let created = self
                .scratch
                .accounts
                .get(address)
                .and_then(Option::as_ref)
                .is_some_and(|account| account.just_created);
            self.delete_account_for_finalization(address)?;
            if created {
                self.scratch.created_selfdestructs.insert(*address);
            }
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

        // Keep the substate sets so that [`Self::build_state_changes`] can derive account status
        // flags; they are cleared with the rest of the scratch by
        // [`Self::clear_transaction_state`].
        self.scratch.selfdestructs = selfdestructs;
        self.scratch.touched = touched;
        Ok(())
    }

    #[inline]
    fn account_changed(
        original: Option<AccountInfoRef<'_>>,
        current: Option<AccountInfoRef<'_>>,
    ) -> bool {
        match (original, current) {
            (Some(original), Some(current)) => {
                original.balance != current.balance
                    || original.nonce != current.nonce
                    || original.code_hash != current.code_hash
            }
            (None, None) => false,
            _ => true,
        }
    }

    #[inline]
    fn changed_code(account: &Account) -> Option<(B256, &Bytecode)> {
        let code_hash = account.code_hash;
        (account.code_changed
            && !account.code.is_empty()
            && !code_hash.is_zero()
            && code_hash != KECCAK256_EMPTY)
            .then_some((code_hash, &account.code))
    }

    #[inline]
    fn storage_slot_changed(storage_wiped: bool, slot: &Tracked<Word>) -> bool {
        slot.is_changed() && (!storage_wiped || !slot.current.is_zero())
    }

    /// Visits transaction state changes in database application order.
    ///
    /// This borrows changes directly from the transaction layer. It does not materialize
    /// [`StateChanges`] and does not mutate the accepted overlay.
    pub(crate) fn visit_transaction_changes<S: StateChangeSink>(
        &self,
        sink: &mut S,
    ) -> Result<(), S::Error> {
        for current in self.scratch.accounts.values().flatten() {
            if let Some((code_hash, code)) = Self::changed_code(current) {
                sink.bytecode(code_hash, code)?;
            }
        }

        for (&address, storage) in &self.scratch.storage {
            if storage.wiped {
                sink.storage_wipe(address)?;
            }
            for (&key, slot) in &storage.slots {
                if Self::storage_slot_changed(storage.wiped, slot) {
                    sink.storage(StorageChange {
                        address,
                        key,
                        original: slot.original,
                        current: slot.current,
                    })?;
                }
            }
        }

        for (&address, current) in &self.scratch.accounts {
            let original = self.database.account_info(&address).map(AccountInfoRef::from_info);
            let current = current.as_ref().map(AccountInfoRef::from_account);
            if Self::account_changed(original, current) {
                sink.account(AccountChangeRef { address, original, current })?;
            }
        }

        Ok(())
    }

    /// Builds the state transition for the current transaction.
    ///
    /// This does not apply changes to the backing database, apply transaction-finalization rules,
    /// take logs, or advance the overlay to the next transaction. Logs are execution output and are
    /// exposed through [`crate::TxResult`] and [`crate::TxResultWithState`].
    pub(crate) fn build_state_changes(&mut self) -> StateChanges {
        let mut changes = StateChanges::default();

        for (address, account) in &self.scratch.accounts {
            let original = self.database.account_info(address).cloned();
            changes.accounts.insert(
                *address,
                AccountChange {
                    original,
                    current: account.as_ref().map(Self::account_change_info),
                    storage: U256Map::default(),
                    wipe_storage: false,
                    created: account.as_ref().is_some_and(|account| account.just_created)
                        || self.scratch.created_selfdestructs.contains(address),
                    selfdestructed: self.scratch.selfdestructs.contains(address),
                },
            );
            if let Some(account) = account
                && let Some((code_hash, code)) = Self::changed_code(account)
            {
                changes.code.entry(code_hash).or_insert_with(|| code.clone());
            }
        }

        for (address, storage) in &self.scratch.storage {
            let entry = changes.accounts.entry(*address).or_insert_with(|| {
                let info = self.database.account_info(address);
                AccountChange {
                    original: info.cloned(),
                    current: info.cloned(),
                    ..AccountChange::default()
                }
            });
            entry.wipe_storage = storage.wiped;
            entry.storage = storage.slots.clone();
        }

        changes
    }

    /// Materializes account info for [`StateChanges`].
    ///
    /// The bytecode is included only when it is actually known so that consumers can distinguish
    /// "no code" from "code not loaded"; changed code is always known.
    fn account_change_info(account: &Account) -> AccountInfo {
        let code = (account.code_changed
            || !account.code.is_empty()
            || account.code_hash == KECCAK256_EMPTY)
            .then(|| account.code.clone());
        AccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code_hash: account.code_hash,
            code,
            _non_exhaustive: (),
        }
    }

    /// Accepts the current transaction's state transition without materializing it.
    pub(crate) fn commit_transaction(&mut self) {
        for current in self.scratch.accounts.values().flatten() {
            if let Some((code_hash, code)) = Self::changed_code(current) {
                self.database.cache.contracts.insert(code_hash, code.clone());
            }
        }

        for (&address, storage) in &self.scratch.storage {
            if storage.wiped {
                self.database.cache.storage.entry(address).or_default().wipe();
            }
            for (&key, slot) in &storage.slots {
                if !Self::storage_slot_changed(storage.wiped, slot) {
                    continue;
                }
                self.database
                    .cache
                    .storage
                    .entry(address)
                    .or_default()
                    .slots
                    .insert(key, slot.current);
            }
        }

        for (&address, current) in &self.scratch.accounts {
            let original = self.database.account_info(&address).map(AccountInfoRef::from_info);
            let current_ref = current.as_ref().map(AccountInfoRef::from_account);
            if !Self::account_changed(original, current_ref) {
                continue;
            }
            match current_ref {
                Some(info) => {
                    self.database.insert_account_info(&address, info.to_account_info_without_code())
                }
                None => {
                    self.database.cache.accounts.insert(address, None);
                    self.database.cache.storage.entry(address).or_default().wipe();
                }
            }
        }

        self.scratch.accounts.clear();
        self.scratch.storage.clear();
    }

    /// Builds and accepts the current transaction's state transition.
    #[cfg(test)]
    pub(crate) fn accept_transaction(&mut self) -> StateChanges {
        let changes = self.build_state_changes();
        self.database.commit_source(&changes);
        self.scratch.accounts.clear();
        self.scratch.storage.clear();
        changes
    }

    /// Marks the current transaction's write layer as accepted state.
    ///
    /// This applies the transaction write-set to the accepted in-memory database overlay and clears
    /// the transaction layer. It does not write to the wrapped backing database; callers remain
    /// responsible for committing the emitted write-set.
    #[cfg(test)]
    pub(crate) fn commit_transaction_overlay(&mut self) {
        let _ = self.accept_transaction();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SpecId, constants::EIP7708_BURN_TOPIC, evm::CacheDB};
    use alloy_primitives::Bytes;

    #[test]
    fn storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &Word::from(1), &Word::from(10));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.set_storage(&address, &Word::from(1), &Word::from(20)).unwrap();
        state.set_storage(&address, &Word::from(1), &Word::from(30)).unwrap();

        assert_eq!(state.read_storage(&address, &Word::from(1)).unwrap(), Word::from(30));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.read_storage(&address, &Word::from(1)).unwrap(), Word::from(10));
    }

    #[test]
    fn transient_storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x22; 20]);
        let mut state = State::new(CacheDB::default());

        state.set_transient_storage(&address, &Word::from(1), &Word::from(10));
        let checkpoint = state.checkpoint();
        state.set_transient_storage(&address, &Word::from(1), &Word::from(20));

        assert_eq!(state.transient_storage(&address, &Word::from(1)), Word::from(20));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.transient_storage(&address, &Word::from(1)), Word::from(10));
    }

    #[test]
    fn destruct_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x33; 20]);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        state.mark_destructed(&address);

        assert!(state.is_selfdestructed(&address));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.is_selfdestructed(&address));
    }

    #[test]
    fn log_rolls_back_to_checkpoint() {
        use alloy_primitives::{Bytes, LogData};

        let mut state = State::new(CacheDB::default());
        let kept = Log {
            address: Address::from([0x44; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x01])),
        };
        let reverted = Log {
            address: Address::from([0x55; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x02])),
        };

        state.log(kept.clone());
        let checkpoint = state.checkpoint();
        state.log(reverted);

        assert_eq!(
            state.logs(),
            &[
                kept.clone(),
                Log {
                    address: Address::from([0x55; 20]),
                    data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x02])),
                }
            ]
        );
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.logs(), &[kept]);
    }

    #[test]
    fn spurious_dragon_rollback_preserves_precompile3_touch() {
        let precompile3 = Address::with_last_byte(3);
        let other = Address::with_last_byte(4);
        let mut database = CacheDB::default();
        database.insert_account_info(&precompile3, AccountInfo::default());
        database.insert_account_info(&other, AccountInfo::default());
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.touch(&precompile3);
        state.touch(&other);

        state.rollback(checkpoint, Version::base(SpecId::SPURIOUS_DRAGON).features);
        assert!(state.scratch.touched.contains(&precompile3));
        assert!(!state.scratch.touched.contains(&other));
    }

    #[test]
    fn non_revertible_warmth_is_not_journaled_or_rolled_back() {
        let base_account = Address::with_last_byte(0x10);
        let frame_account = Address::with_last_byte(0x11);
        let base_storage = Address::with_last_byte(0x12);
        let frame_storage = Address::with_last_byte(0x13);
        let key = Word::from(1);
        let mut state = State::new(CacheDB::default());

        state.warm_account_non_revertible(&base_account);
        assert!(state.warm_storage_non_revertible(&base_storage, &key));
        assert!(state.scratch.journal.is_empty());

        let checkpoint = state.checkpoint();
        assert!(state.warm_account(&frame_account));
        assert!(state.warm_storage(&frame_storage, &key));
        assert_eq!(state.scratch.journal.len(), 2);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.is_account_warm(&base_account));
        assert!(state.is_storage_warm(&base_storage, &key));
        assert!(!state.is_account_warm(&frame_account));
        assert!(!state.is_storage_warm(&frame_storage, &key));
    }

    #[test]
    fn read_committed_storage_ignores_transaction_overlay() {
        let address = Address::with_last_byte(0x11);
        let key = Word::from(0x22);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        database.insert_account_storage(&address, &key, &Word::from(1));
        let mut state = State::new(database);

        let _ = state.set_storage(&address, &key, &Word::from(2)).unwrap();

        assert_eq!(state.read_storage(&address, &key).unwrap(), Word::from(2));
        assert_eq!(state.read_committed_storage(&address, &key).unwrap(), Word::from(1));
    }

    #[test]
    fn build_state_changes_leaves_logs_on_transaction_state() {
        use alloy_primitives::{Bytes, LogData};

        let mut state = State::new(CacheDB::default());
        let log = Log {
            address: Address::from([0x66; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x03])),
        };

        state.log(log.clone());
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();
        assert!(changes.is_empty());
        assert_eq!(state.logs(), core::slice::from_ref(&log));

        state.commit_transaction_overlay();
        state.clear_transaction_state();
        assert!(state.logs().is_empty());
    }

    #[test]
    fn transfer_to_self_requires_balance() {
        let address = Address::from([0x77; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(3)));
        let mut state = State::new(database);

        assert!(!state.transfer(&address, &address, &Word::from(4)).unwrap());
        assert!(state.transfer(&address, &address, &Word::from(3)).unwrap());
    }

    #[test]
    fn spurious_dragon_deletes_touched_empty_existing_account() {
        let address = Address::from([0x44; 20]);
        let empty = AccountInfo { code: None, ..AccountInfo::default() };
        let mut database = CacheDB::default();
        database.insert_account_info(&address, empty.clone());
        let mut state = State::new(database);

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        let change = changes.accounts.get(&address).expect("touched empty account is deleted");
        assert_eq!(change.original, Some(empty));
        assert_eq!(change.current, None);
        assert!(change.is_storage_wiped());
    }

    #[test]
    fn homestead_preserves_touched_empty_existing_account() {
        let address = Address::from([0x45; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default());
        let mut state = State::new(database);

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::HOMESTEAD));
        let changes = state.build_state_changes();

        assert!(changes.accounts.get(&address).is_none_or(|change| !change.is_changed()));
    }

    #[test]
    fn homestead_materializes_touched_empty_new_account() {
        let address = Address::from([0x46; 20]);
        let mut state = State::new(CacheDB::default());

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::HOMESTEAD));
        let changes = state.build_state_changes();

        let change =
            changes.accounts.get(&address).expect("pre-spurious empty touch creates account");
        assert_eq!(change.original, None);
        let current = change.current.as_ref().expect("created empty account");
        assert!(current.is_empty());
    }

    #[test]
    fn spurious_dragon_ignores_touched_empty_new_account() {
        let address = Address::from([0x47; 20]);
        let mut state = State::new(CacheDB::default());

        state.touch(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        assert!(changes.accounts.get(&address).is_none_or(|change| !change.is_changed()));
    }

    #[test]
    fn finalization_preserves_touched_set_capacity() {
        let mut state = State::new(CacheDB::default());

        for i in 0..32 {
            state.touch(&Address::from([i; 20]));
            state.mark_destructed(&Address::from([i + 32; 20]));
        }

        let touched_capacity = state.scratch.touched.capacity();
        let selfdestructs_capacity = state.scratch.selfdestructs.capacity();

        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));

        // The substate sets are preserved for `build_state_changes` and only cleared together
        // with the rest of the transaction scratch.
        assert_eq!(state.scratch.touched.capacity(), touched_capacity);
        assert_eq!(state.scratch.selfdestructs.capacity(), selfdestructs_capacity);

        state.clear_transaction_state();
        assert!(state.scratch.touched.is_empty());
        assert!(state.scratch.selfdestructs.is_empty());
        assert_eq!(state.scratch.touched.capacity(), touched_capacity);
        assert_eq!(state.scratch.selfdestructs.capacity(), selfdestructs_capacity);
    }

    #[test]
    fn build_state_changes_deduplicates_code() {
        let code = Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[0x00]));
        let code_hash = code.hash_slow();
        let first = Address::from([0x48; 20]);
        let second = Address::from([0x49; 20]);
        let mut state = State::new(CacheDB::default());

        state.set_code(&first, code.clone()).unwrap();
        state.set_code(&second, code.clone()).unwrap();
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        assert_eq!(changes.code.len(), 1);
        assert_eq!(changes.code.get(&code_hash), Some(&code));
        for address in [first, second] {
            let current = changes.accounts.get(&address).unwrap().current.as_ref().unwrap();
            assert_eq!(current.code_hash, code_hash);
            assert_eq!(current.code.as_ref(), Some(&code));
        }
    }

    #[test]
    fn selfdestruct_deletes_account_and_wipes_storage() {
        let address = Address::from([0x48; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(&address, &Word::from(1), &Word::from(2));
        let mut state = State::new(database);

        state.mark_destructed(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        let change = changes.accounts.get(&address).expect("selfdestruct deletes account");
        assert!(change.original.is_some());
        assert_eq!(change.current, None);
        assert!(change.is_selfdestructed());
        assert!(change.is_storage_wiped());
    }

    #[test]
    fn code_respects_deleted_transaction_overlay() {
        let address = Address::from([0x4a; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[0x00]));
        let mut database = CacheDB::default();
        database.insert_account_info(&address, AccountInfo::default().with_code(code.clone()));
        let mut state = State::new(database);

        assert_eq!(state.read_code(&address).unwrap(), code);
        state.mark_destructed(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));

        assert_eq!(state.read_code(&address).unwrap(), Bytecode::default());
        assert_eq!(state.read_code(&address).unwrap(), Bytecode::default());
    }

    #[test]
    fn selfdestruct_emits_created_account_deletion() {
        let caller = Address::from([0x48; 20]);
        let address = Address::from([0x49; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        state
            .create_account(
                &caller,
                &address,
                &Word::ZERO,
                Version::base(crate::SpecId::SPURIOUS_DRAGON).features,
            )
            .unwrap()
            .unwrap();
        state.mark_destructed(&address);
        state.finalize_transaction_(Version::base(crate::SpecId::SPURIOUS_DRAGON));
        let changes = state.build_state_changes();

        let change = changes.accounts.get(&address).expect("created selfdestruct is deleted");
        assert_eq!(change.original, None);
        assert_eq!(change.current, None);
        assert!(change.is_created());
        assert!(change.is_selfdestructed());
    }

    #[test]
    fn eip7708_delayed_burn_logs_selfdestructs_sorted() {
        let high = Address::from([0x22; 20]);
        let low = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(&high, AccountInfo::default().with_balance(Word::from(2)));
        database.insert_account_info(&low, AccountInfo::default().with_balance(Word::from(1)));
        let mut state = State::new(database);

        state.mark_destructed(&high);
        state.mark_destructed(&low);
        let mut inspected = Vec::new();
        state
            .finalize_transaction(Version::base(SpecId::AMSTERDAM), |log| {
                inspected.push(log.clone())
            })
            .unwrap();

        let logs = state.take_logs();
        assert_eq!(inspected, logs);
        assert_eq!(logs.len(), 2);
        assert_eq!(
            logs[0].topics(),
            &[EIP7708_BURN_TOPIC, B256::left_padding_from(low.as_slice())]
        );
        assert_eq!(logs[0].data.data, Bytes::copy_from_slice(&Word::from(1).to_be_bytes::<32>()));
        assert_eq!(
            logs[1].topics(),
            &[EIP7708_BURN_TOPIC, B256::left_padding_from(high.as_slice())]
        );
        assert_eq!(logs[1].data.data, Bytes::copy_from_slice(&Word::from(2).to_be_bytes::<32>()));
    }
}
