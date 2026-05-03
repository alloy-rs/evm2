//! Basic in-memory EVM host state.

use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{collections::BTreeMap, string::ToString, vec::Vec};
use alloy_primitives::{
    Address, B256, U256, keccak256,
    map::{self, HashMap, HashSet, hash_map},
};

pub use alloy_primitives::KECCAK256_EMPTY as KECCAK_EMPTY;

/// A value tracked together with the value it had at the start of the current
/// transaction.
///
/// `Tracked` is used internally by [`State`] to keep an overlay over the
/// backing database. `original` is the value at the current transaction
/// boundary, while `current` is the value after all in-flight EVM mutations.
/// When a transaction is accepted, `current` becomes the next transaction's
/// `original` without writing anything to the backing database.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Tracked<T> {
    /// Value at the start of the current transaction.
    pub original: T,
    /// Current overlay value.
    pub current: T,
}

impl<T> Tracked<T> {
    /// Creates a tracked value whose original and current values are equal.
    #[inline]
    pub fn new(value: T) -> Self
    where
        T: Clone,
    {
        Self { original: value.clone(), current: value }
    }
}

impl<T: PartialEq> Tracked<T> {
    /// Returns whether the current value differs from the original value.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.original != self.current
    }
}

/// Account information loaded from the backing database or emitted in a state
/// transition.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct AccountInfo {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Hash of the raw bytes in `code`, or the empty code hash.
    pub code_hash: B256,
    /// Bytecode associated with this account.
    pub code: Option<Bytecode>,
}

impl Default for AccountInfo {
    #[inline]
    fn default() -> Self {
        Self {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            code: Some(Bytecode::default()),
        }
    }
}

impl AccountInfo {
    /// Creates a new [`AccountInfo`] with the given fields.
    #[inline]
    pub const fn new(balance: Word, nonce: u64, code_hash: B256, code: Bytecode) -> Self {
        Self { balance, nonce, code_hash, code: Some(code) }
    }

    /// Creates a new [`AccountInfo`] with the given code.
    #[inline]
    pub fn with_code(self, code: Bytecode) -> Self {
        Self { code_hash: code.hash_slow(), code: Some(code), ..self }
    }

    /// Creates a new [`AccountInfo`] with the given balance.
    #[inline]
    pub const fn with_balance(mut self, balance: Word) -> Self {
        self.balance = balance;
        self
    }

    /// Creates a new [`AccountInfo`] with the given nonce.
    #[inline]
    pub const fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = nonce;
        self
    }

    /// Sets account bytecode and updates the code hash.
    #[inline]
    pub fn set_code(&mut self, code: Bytecode) {
        self.code_hash = code.hash_slow();
        self.code = Some(code);
    }

    /// Returns whether this account is empty by the Spurious Dragon definition.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.balance.is_zero() && self.nonce == 0 && self.code_hash == KECCAK_EMPTY
    }
}

type StorageValue = Tracked<Word>;

/// Mutable account state cached by [`State`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Account {
    /// Account nonce.
    pub nonce: u64,
    /// Account balance.
    pub balance: Word,
    /// Account code hash.
    pub code_hash: B256,
    /// EIP-1153 transient storage.
    pub transient_storage: HashMap<Word, Word>,
    /// Cached account bytecode.
    pub code: Bytecode,
    /// Whether the account was created in the current transaction.
    pub just_created: bool,
    /// Whether the account code has been modified.
    pub code_changed: bool,
}

impl Account {
    /// Creates an account from database account info.
    #[inline]
    pub fn from_info(info: AccountInfo) -> Self {
        Self {
            nonce: info.nonce,
            balance: info.balance,
            code_hash: info.code_hash,
            code: info.code.unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Returns account info.
    #[inline]
    pub fn info(&self) -> AccountInfo {
        AccountInfo {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: Some(self.code.clone()),
        }
    }

    /// Returns whether this account is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nonce == 0 && self.balance.is_zero() && self.code_hash == KECCAK_EMPTY
    }
}

/// Persistent storage overlay for one account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct StorageOverlay {
    /// Whether consumers must delete all pre-existing storage for the account
    /// before applying individual slot changes.
    pub wiped: bool,
    /// Loaded or changed storage slots.
    pub(crate) slots: HashMap<Word, StorageValue>,
}

/// Complete state transition produced by a transaction.
///
/// `StateChanges` is the public write-set returned in [`crate::TxResult`]. It
/// is intentionally explicit so embedding clients can update their own database
/// and compute post-state roots without reimplementing EVM account-lifetime
/// rules.
///
/// Consumers should apply changes in this order:
///
/// 1. write bytecode from [`Self::code`] for every non-empty code hash they do not already have;
/// 2. for each [`StorageChangeSet`] whose [`StorageChangeSet::wipe`] flag is true, delete all
///    storage for that account;
/// 3. apply each storage slot change: a zero [`StorageChange::current`] means delete the slot,
///    otherwise write the slot value;
/// 4. apply account changes: `current = Some(..)` means upsert the account, `current = None` means
///    delete the account.
///
/// `evm2` does not write to the backing database. These changes describe what
/// happened; applying them is the responsibility of the caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateChanges {
    /// Account changes keyed by address.
    pub accounts: BTreeMap<Address, AccountChange>,
    /// Persistent storage changes keyed by account address.
    pub storage: BTreeMap<Address, StorageChangeSet>,
    /// Newly created or modified bytecode keyed by code hash.
    pub code: BTreeMap<B256, Bytecode>,
}

impl StateChanges {
    /// Returns whether this transition contains no database-visible changes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty() && self.code.is_empty()
    }
}

/// Account transition for a single address.
///
/// `original` is the account at the beginning of the transaction. `current` is
/// the account after transaction execution and EVM account-lifetime rules have
/// been evaluated. `current = None` is an explicit account deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountChange {
    /// Account at the beginning of the transaction.
    pub original: Option<AccountInfo>,
    /// Account after the transaction, or `None` to delete it.
    pub current: Option<AccountInfo>,
}

/// Storage transition for a single account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageChangeSet {
    /// If true, delete all pre-existing storage for this account before applying
    /// [`Self::slots`]. This is used for selfdestruct and contract
    /// re-incarnation semantics using an explicit storage wipe marker.
    pub wipe: bool,
    /// Changed storage slots keyed by slot.
    pub slots: BTreeMap<Word, StorageChange>,
}

/// Storage transition for a single slot.
///
/// `original` is the slot value at the beginning of the transaction, after any
/// storage wipe/re-incarnation semantics that occurred before the slot was
/// loaded. `current = 0` means the consumer should delete the slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageChange {
    /// Slot value at the beginning of the transaction.
    pub original: Word,
    /// Slot value after the transaction. Zero means delete the slot.
    pub current: Word,
}

/// Backing database view used to initialize mutable [`State`].
pub trait Database {
    /// Loads account information.
    fn get_account(&self, address: Address) -> Option<AccountInfo>;

    /// Loads account code.
    fn get_account_code(&self, address: Address) -> Bytecode;

    /// Loads a persistent storage slot.
    fn get_storage(&self, address: Address, key: Word) -> Word;

    /// Loads a historical block hash.
    fn get_block_hash(&self, number: u64) -> Option<B256>;
}

/// Compact journal entry for reverting state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JournalEntry {
    /// Account current value changed.
    AccountChange {
        /// Account address.
        address: Address,
        /// Previous current account value.
        previous: Option<Account>,
    },
    /// Account overlay entry was inserted.
    AccountInserted {
        /// Account address.
        address: Address,
    },
    /// Account was touched.
    Touch {
        /// Account address.
        address: Address,
    },
    /// Account was self-destructed.
    SelfDestruct {
        /// Account address.
        address: Address,
    },
    /// Persistent storage changed.
    StorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous current storage value.
        previous: Word,
    },
    /// Persistent storage slot overlay was inserted.
    StorageInserted {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
    },
    /// Account storage wipe flag changed.
    StorageWipe {
        /// Account address.
        address: Address,
        /// Previous storage overlay.
        previous: Option<StorageOverlay>,
    },
    /// Transient storage changed.
    TransientStorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous transient storage value.
        previous: Option<Word>,
    },
    /// Account was warmed by EIP-2929 access tracking.
    AccountWarmed {
        /// Account address.
        address: Address,
    },
    /// Storage slot was warmed by EIP-2929 access tracking.
    StorageWarmed {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
    },
}

/// Mutable EVM state with an overlay and reversible journal.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct State<D> {
    /// Read-only initial database.
    pub initial: D,
    /// Account data overlay keyed by address.
    ///
    /// Entries are created when account state is loaded or mutated. The tracked
    /// `original`/`current` pair is used to execute against the in-memory overlay
    /// and later derive account-level [`StateChanges`]. Presence here does not by
    /// itself mean the account was touched or warmed.
    pub accounts: HashMap<Address, Tracked<Option<Account>>>,
    /// Persistent storage overlay keyed by account address.
    pub storage: HashMap<Address, StorageOverlay>,
    /// Revert journal.
    pub journal: Vec<JournalEntry>,
    /// Accounts touched for transaction-finalization account-lifetime rules.
    ///
    /// This is separate from the account overlay and the EIP-2929 warm set. A
    /// touched account may have no field changes, but can still matter for empty
    /// account deletion/materialization rules across forks.
    pub touched: HashSet<Address>,
    /// Accounts self-destructed in the current transaction.
    pub selfdestructs: HashSet<Address>,
    /// Transaction-scoped warm account set for EIP-2929 gas accounting.
    ///
    /// This tracks whether account access is warm or cold. It does not imply the
    /// account was touched, changed, or should be emitted in [`StateChanges`].
    pub accessed_accounts: HashSet<Address>,
    /// Transaction-scoped warm storage slot set.
    pub accessed_storage: HashSet<(Address, Word)>,
}

impl<D> State<D> {
    /// Creates a new state over an initial database.
    #[inline]
    pub fn new(initial: D) -> Self {
        Self {
            initial,
            accounts: map::HashMap::default(),
            storage: map::HashMap::default(),
            journal: Vec::new(),
            touched: map::HashSet::default(),
            selfdestructs: map::HashSet::default(),
            accessed_accounts: map::HashSet::default(),
            accessed_storage: map::HashSet::default(),
        }
    }

    /// Returns a checkpoint for later rollback.
    #[inline]
    pub const fn checkpoint(&self) -> usize {
        self.journal.len()
    }

    /// Returns the initial database.
    #[inline]
    pub const fn initial(&self) -> &D {
        &self.initial
    }

    /// Returns the initial database mutably.
    #[inline]
    pub const fn initial_mut(&mut self) -> &mut D {
        &mut self.initial
    }

    /// Returns the current account overlay if present and not deleted.
    #[inline]
    pub fn account_ref(&self, address: Address) -> Option<&Account> {
        self.accounts.get(&address).and_then(|account| account.current.as_ref())
    }

    /// Returns whether an account is warm in the current transaction.
    #[inline]
    pub fn is_account_warm(&self, address: Address) -> bool {
        self.accessed_accounts.contains(&address)
    }

    /// Marks an account as warm and returns whether it was cold before this access.
    #[inline]
    pub fn warm_account(&mut self, address: Address) -> bool {
        if self.accessed_accounts.insert(address) {
            self.journal.push(JournalEntry::AccountWarmed { address });
            true
        } else {
            false
        }
    }

    /// Marks a storage slot as warm and returns whether it was cold before this access.
    #[inline]
    pub fn warm_storage(&mut self, address: Address, key: Word) -> bool {
        if self.accessed_storage.insert((address, key)) {
            self.journal.push(JournalEntry::StorageWarmed { address, key });
            true
        } else {
            false
        }
    }

    /// Clears transaction-scoped substate.
    #[inline]
    pub fn clear_transaction_state(&mut self) {
        self.journal.clear();
        self.touched.clear();
        self.selfdestructs.clear();
        self.accessed_accounts.clear();
        self.accessed_storage.clear();
    }

    /// Clears all transaction-scoped warm accesses.
    #[inline]
    pub fn clear_accesses(&mut self) {
        self.accessed_accounts.clear();
        self.accessed_storage.clear();
    }
}

impl<D: Database> State<D> {
    fn load_account(&mut self, address: Address) -> Option<&Tracked<Option<Account>>> {
        if !self.accounts.contains_key(&address)
            && let Some(info) = self.initial.get_account(address)
        {
            let account = Account::from_info(info);
            self.accounts.insert(address, Tracked::new(Some(account)));
        }
        self.accounts.get(&address)
    }

    fn ensure_account_overlay(&mut self, address: Address) {
        if !self.accounts.contains_key(&address) {
            let original = self.initial.get_account(address).map(Account::from_info);
            self.accounts
                .insert(address, Tracked { original: original.clone(), current: original });
            self.journal.push(JournalEntry::AccountInserted { address });
        }
    }

    fn account_mut(&mut self, address: Address) -> &mut Account {
        self.ensure_account_overlay(address);
        if self.accounts[&address].current.is_none() {
            self.journal.push(JournalEntry::AccountChange { address, previous: None });
            self.accounts.get_mut(&address).expect("account overlay exists").current =
                Some(Account {
                    code_hash: KECCAK_EMPTY,
                    code: Bytecode::default(),
                    ..Account::default()
                });
        }
        self.accounts
            .get_mut(&address)
            .and_then(|account| account.current.as_mut())
            .expect("current account exists")
    }

    fn journal_account_change(&mut self, address: Address) {
        self.ensure_account_overlay(address);
        let previous = self.accounts[&address].current.clone();
        self.journal.push(JournalEntry::AccountChange { address, previous });
    }

    /// Returns account info.
    #[inline]
    pub fn account_info(&self, address: Address) -> Option<AccountInfo> {
        if let Some(account) = self.accounts.get(&address) {
            return account.current.as_ref().map(Account::info);
        }
        self.initial.get_account(address)
    }

    /// Returns an account if it exists.
    #[inline]
    pub fn find(&mut self, address: Address) -> Option<&Account> {
        self.load_account(address);
        self.account_ref(address)
    }

    /// Gets an existing account or inserts a new empty account.
    #[inline]
    pub fn get_or_insert(&mut self, address: Address) -> &mut Account {
        self.account_mut(address)
    }

    /// Gets account code.
    #[inline]
    pub fn get_code(&mut self, address: Address) -> Bytecode {
        let Some(account) = self.find(address) else {
            return Bytecode::default();
        };
        if account.code_hash == KECCAK_EMPTY {
            return Bytecode::default();
        }
        if !account.code.is_empty() {
            return account.code.clone();
        }
        self.initial.get_account_code(address)
    }

    fn storage_initial(&self, address: Address, key: Word) -> Word {
        if self.storage.get(&address).is_some_and(|storage| storage.wiped)
            || self.accounts.get(&address).is_some_and(|account| account.original.is_none())
        {
            Word::ZERO
        } else {
            self.initial.get_storage(address, key)
        }
    }

    fn ensure_storage_slot(&mut self, address: Address, key: Word, journal_insert: bool) {
        let initial = self.storage_initial(address, key);
        let storage = self.storage.entry(address).or_default();
        if let hash_map::Entry::Vacant(entry) = storage.slots.entry(key) {
            entry.insert(StorageValue::new(initial));
            if journal_insert {
                self.journal.push(JournalEntry::StorageInserted { address, key });
            }
        }
    }

    #[inline]
    fn get_storage_value(&mut self, address: Address, key: Word) -> &mut StorageValue {
        self.ensure_storage_slot(address, key, false);
        self.storage
            .get_mut(&address)
            .and_then(|storage| storage.slots.get_mut(&key))
            .expect("storage slot was just inserted")
    }

    /// Loads persistent storage.
    #[inline]
    pub fn storage(&mut self, address: Address, key: Word) -> Word {
        if self.account_info(address).is_none() {
            return Word::ZERO;
        }
        self.get_storage_value(address, key).current
    }

    /// Stores persistent storage.
    #[inline]
    pub fn set_storage(&mut self, address: Address, key: Word, value: Word) {
        self.account_mut(address);
        let inserted =
            !self.storage.get(&address).is_some_and(|storage| storage.slots.contains_key(&key));
        self.ensure_storage_slot(address, key, inserted);
        let previous = self.storage[&address].slots[&key].current;
        self.journal.push(JournalEntry::StorageChange { address, key, previous });
        self.storage
            .get_mut(&address)
            .expect("storage overlay exists")
            .slots
            .get_mut(&key)
            .expect("storage slot exists")
            .current = value;
    }

    /// Marks an account as touched by the current transaction.
    #[inline]
    pub fn touch(&mut self, address: Address) {
        if self.touched.insert(address) {
            self.journal.push(JournalEntry::Touch { address });
        }
    }

    /// Adds a signed balance delta by wrapping two's-complement values.
    #[inline]
    pub fn add_balance(&mut self, address: Address, delta: Word) {
        if delta.is_zero() {
            self.touch(address);
            return;
        }
        self.journal_account_change(address);
        let account = self.account_mut(address);
        account.balance = account.balance.wrapping_add(delta);
        self.touch(address);
    }

    /// Transfers value between accounts.
    #[inline]
    pub fn transfer(&mut self, from: Address, to: Address, value: Word) -> bool {
        if value.is_zero() || from == to {
            self.touch(to);
            return true;
        }

        let from_balance = self.account_info(from).map_or(Word::ZERO, |info| info.balance);
        let Some(new_from_balance) = from_balance.checked_sub(value) else {
            return false;
        };

        self.journal_account_change(from);
        self.account_mut(from).balance = new_from_balance;
        self.touch(from);

        self.journal_account_change(to);
        let to_balance = self.account_mut(to).balance;
        self.account_mut(to).balance = to_balance.saturating_add(value);
        self.touch(to);
        true
    }

    /// Increments account nonce.
    #[inline]
    pub fn increment_nonce(&mut self, address: Address) {
        self.journal_account_change(address);
        let account = self.account_mut(address);
        account.nonce = account.nonce.saturating_add(1);
        self.touch(address);
    }

    /// Creates a contract account and transfers endowment from the caller.
    #[inline]
    pub fn create_account(
        &mut self,
        caller: Address,
        address: Address,
        value: Word,
        spec: crate::SpecId,
    ) -> Result<(), crate::interpreter::InstrStop> {
        if let Some(info) = self.account_info(address)
            && (info.nonce != 0 || info.code_hash != KECCAK_EMPTY)
        {
            return Err(crate::interpreter::InstrStop::CreateCollision);
        }

        if !self.transfer(caller, address, value) {
            return Err(crate::interpreter::InstrStop::OutOfFunds);
        }

        let balance = self.account_mut(address).balance;
        self.wipe_storage(address);
        self.journal_account_change(address);
        self.accounts.get_mut(&address).expect("account overlay exists").current = Some(Account {
            nonce: u64::from(spec.enables(crate::SpecId::SPURIOUS_DRAGON)),
            balance,
            code_hash: KECCAK_EMPTY,
            code: Bytecode::default(),
            just_created: true,
            code_changed: true,
            ..Account::default()
        });
        self.touch(address);
        Ok(())
    }

    /// Sets account bytecode.
    #[inline]
    pub fn set_code(&mut self, address: Address, code: Bytecode) {
        self.journal_account_change(address);
        let account = self.account_mut(address);
        account.code_hash = code.hash_slow();
        account.code = code;
        account.code_changed = true;
    }

    /// Marks all prior persistent storage for `address` as deleted.
    #[inline]
    pub fn wipe_storage(&mut self, address: Address) {
        let previous = self.storage.get(&address).cloned();
        self.journal.push(JournalEntry::StorageWipe { address, previous });
        self.storage
            .insert(address, StorageOverlay { wiped: true, slots: map::HashMap::default() });
    }

    /// Loads transient storage.
    #[inline]
    pub fn transient_storage(&mut self, address: Address, key: Word) -> Word {
        self.account_ref(address)
            .and_then(|account| account.transient_storage.get(&key).copied())
            .unwrap_or_default()
    }

    /// Stores transient storage.
    #[inline]
    pub fn set_transient_storage(&mut self, address: Address, key: Word, value: Word) {
        let previous = self.account_mut(address).transient_storage.get(&key).copied();
        self.journal.push(JournalEntry::TransientStorageChange { address, key, previous });
        self.account_mut(address).transient_storage.insert(key, value);
    }

    /// Marks an account as self-destructed in the current transaction.
    #[inline]
    pub fn mark_destructed(&mut self, address: Address) {
        if self.selfdestructs.insert(address) {
            self.journal.push(JournalEntry::SelfDestruct { address });
        }
        self.touch(address);
    }

    /// Returns whether an account has been marked self-destructed in the current transaction.
    #[inline]
    pub fn is_selfdestructed(&self, address: Address) -> bool {
        self.selfdestructs.contains(&address)
    }

    /// Reverts state changes after the checkpoint.
    #[inline]
    pub fn rollback(&mut self, checkpoint: usize) {
        while self.journal.len() != checkpoint {
            match self.journal.pop().expect("journal length checked above") {
                JournalEntry::AccountChange { address, previous } => {
                    if let Some(account) = self.accounts.get_mut(&address) {
                        account.current = previous;
                    }
                }
                JournalEntry::AccountInserted { address } => {
                    self.accounts.remove(&address);
                }
                JournalEntry::Touch { address } => {
                    self.touched.remove(&address);
                }
                JournalEntry::SelfDestruct { address } => {
                    self.selfdestructs.remove(&address);
                }
                JournalEntry::StorageChange { address, key, previous } => {
                    if let Some(storage) = self.storage.get_mut(&address)
                        && let Some(slot) = storage.slots.get_mut(&key)
                    {
                        slot.current = previous;
                    }
                }
                JournalEntry::StorageInserted { address, key } => {
                    if let Some(storage) = self.storage.get_mut(&address) {
                        storage.slots.remove(&key);
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
                JournalEntry::TransientStorageChange { address, key, previous } => {
                    let storage = &mut self.account_mut(address).transient_storage;
                    match previous {
                        Some(previous) => {
                            storage.insert(key, previous);
                        }
                        None => {
                            storage.remove(&key);
                        }
                    }
                }
                JournalEntry::AccountWarmed { address } => {
                    self.accessed_accounts.remove(&address);
                }
                JournalEntry::StorageWarmed { address, key } => {
                    self.accessed_storage.remove(&(address, key));
                }
            }
        }
    }

    fn final_account_info(account: &Account, deleted: bool) -> Option<AccountInfo> {
        if deleted { None } else { Some(account.info()) }
    }

    fn account_deleted_by_rules(
        &self,
        address: Address,
        account: Option<&Account>,
        spec: crate::SpecId,
    ) -> bool {
        if self.selfdestructs.contains(&address) {
            return true;
        }
        spec.enables(crate::SpecId::SPURIOUS_DRAGON)
            && self.touched.contains(&address)
            && account.is_none_or(Account::is_empty)
    }

    /// Builds the database-visible state transition for the current transaction.
    ///
    /// This method is pure: it does not apply changes to the backing database and
    /// does not advance the overlay to the next transaction. Callers normally use
    /// the [`StateChanges`] attached to [`crate::TxResult`] instead of invoking
    /// this directly.
    pub fn build_state_changes(&self, spec: crate::SpecId) -> StateChanges {
        let mut changes = StateChanges::default();

        for (&address, tracked) in &self.accounts {
            let deleted = self.account_deleted_by_rules(address, tracked.current.as_ref(), spec);
            let original = tracked.original.as_ref().map(Account::info);
            let current = tracked
                .current
                .as_ref()
                .and_then(|account| Self::final_account_info(account, deleted));
            if original != current {
                changes.accounts.insert(address, AccountChange { original, current });
            }
            if let Some(account) = tracked.current.as_ref()
                && account.code_changed
                && !account.code.is_empty()
                && !account.code_hash.is_zero()
                && account.code_hash != KECCAK_EMPTY
            {
                changes.code.insert(account.code_hash, account.code.clone());
            }
        }

        for &address in &self.touched {
            if self.accounts.contains_key(&address) {
                continue;
            }
            let original = self.initial.get_account(address);
            if spec.enables(crate::SpecId::SPURIOUS_DRAGON) {
                if original.as_ref().is_some_and(AccountInfo::is_empty) {
                    changes.accounts.insert(address, AccountChange { original, current: None });
                }
                continue;
            }
            if original.is_none() {
                let empty = Account {
                    code_hash: KECCAK_EMPTY,
                    code: Bytecode::default(),
                    ..Account::default()
                };
                changes
                    .accounts
                    .insert(address, AccountChange { original: None, current: Some(empty.info()) });
            }
        }

        for &address in &self.selfdestructs {
            let original = self
                .accounts
                .get(&address)
                .and_then(|tracked| tracked.original.as_ref())
                .map(Account::info)
                .or_else(|| self.initial.get_account(address));
            changes.accounts.entry(address).or_insert(AccountChange { original, current: None });
        }

        for (&address, storage) in &self.storage {
            let account_deleted =
                changes.accounts.get(&address).is_some_and(|change| change.current.is_none());
            let mut set =
                StorageChangeSet { wipe: storage.wiped || account_deleted, slots: BTreeMap::new() };
            if !account_deleted {
                for (&key, slot) in &storage.slots {
                    if slot.original != slot.current && (!set.wipe || !slot.current.is_zero()) {
                        set.slots.insert(
                            key,
                            StorageChange { original: slot.original, current: slot.current },
                        );
                    }
                }
            }
            if set.wipe || !set.slots.is_empty() {
                changes.storage.insert(address, set);
            }
        }

        for (&address, change) in &changes.accounts {
            if change.current.is_none() {
                changes.storage.entry(address).or_default().wipe = true;
            }
        }

        changes
    }

    /// Marks the current transaction's overlay values as the new baseline.
    ///
    /// After [`Self::build_state_changes`] has emitted the transaction write-set,
    /// the overlay must stop treating those writes as pending changes. This rolls
    /// the current account and storage values forward into their `original` slots,
    /// applies local deletion/wipe bookkeeping, and clears transaction-local journal,
    /// touch, selfdestruct, and access-list state. It only advances the in-memory
    /// overlay; callers are still responsible for applying the emitted write-set to
    /// their backing database.
    pub(super) fn accept_transaction(&mut self, spec: crate::SpecId) {
        let changes = self.build_state_changes(spec);
        for (&address, change) in &changes.accounts {
            self.ensure_account_overlay(address);
            let current = change.current.clone().map(Account::from_info);
            if let Some(account) = self.accounts.get_mut(&address) {
                account.original = current.clone();
                account.current = current;
            }
        }
        for (&address, change) in &changes.storage {
            if change.wipe {
                self.storage.remove(&address);
            }
            if !change.slots.is_empty() {
                let storage = self.storage.entry(address).or_default();
                for (&key, slot_change) in &change.slots {
                    storage.slots.insert(key, StorageValue::new(slot_change.current));
                }
            }
        }
        for storage in self.storage.values_mut() {
            for slot in storage.slots.values_mut() {
                slot.original = slot.current;
            }
            storage.wiped = false;
        }
        self.clear_transaction_state();
    }
}

/// Returns the state-test logs hash.
pub fn logs_hash(logs: &[alloy_primitives::Log]) -> B256 {
    let mut out = Vec::with_capacity(alloy_rlp::list_length(logs));
    alloy_rlp::encode_list(logs, &mut out);
    keccak256(out)
}

/// A simple in-memory database view.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheDB {
    /// Accounts keyed by address.
    pub accounts: HashMap<Address, AccountInfo>,
    /// Contracts keyed by code hash.
    pub contracts: HashMap<B256, Bytecode>,
    /// Persistent storage keyed by account and slot.
    pub storage: HashMap<(Address, Word), Word>,
    /// Cached block hashes keyed by block number.
    pub block_hashes: HashMap<u64, B256>,
}

impl Default for CacheDB {
    #[inline]
    fn default() -> Self {
        let mut contracts = map::HashMap::default();
        contracts.insert(KECCAK_EMPTY, Bytecode::default());
        contracts.insert(B256::ZERO, Bytecode::default());

        Self {
            accounts: map::HashMap::default(),
            contracts,
            storage: map::HashMap::default(),
            block_hashes: map::HashMap::default(),
        }
    }
}

impl CacheDB {
    /// Inserts account code into the contract cache.
    #[inline]
    pub fn insert_contract(&mut self, info: &mut AccountInfo) {
        if let Some(code) = &info.code
            && !code.is_empty()
        {
            if info.code_hash == KECCAK_EMPTY {
                info.code_hash = code.hash_slow();
            }
            self.contracts.entry(info.code_hash).or_insert_with(|| code.clone());
        }
        if info.code_hash.is_zero() {
            info.code_hash = KECCAK_EMPTY;
        }
    }

    /// Inserts account info.
    #[inline]
    pub fn insert_account_info(&mut self, address: Address, mut info: AccountInfo) {
        self.insert_contract(&mut info);
        self.accounts.insert(address, info);
    }

    /// Returns account info if the account exists.
    #[inline]
    pub fn account_info(&self, address: Address) -> Option<&AccountInfo> {
        self.accounts.get(&address)
    }

    /// Inserts persistent storage.
    #[inline]
    pub fn insert_account_storage(&mut self, address: Address, key: Word, value: Word) {
        self.accounts.entry(address).or_default();
        self.storage.insert((address, key), value);
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }
}

impl Database for CacheDB {
    #[inline]
    fn get_account(&self, address: Address) -> Option<AccountInfo> {
        self.accounts.get(&address).cloned()
    }

    #[inline]
    fn get_account_code(&self, address: Address) -> Bytecode {
        self.accounts
            .get(&address)
            .and_then(|info| info.code.clone())
            .or_else(|| {
                self.accounts
                    .get(&address)
                    .and_then(|info| self.contracts.get(&info.code_hash).cloned())
            })
            .unwrap_or_default()
    }

    #[inline]
    fn get_storage(&self, address: Address, key: Word) -> Word {
        self.storage.get(&(address, key)).copied().unwrap_or_default()
    }

    #[inline]
    fn get_block_hash(&self, number: u64) -> Option<B256> {
        self.block_hashes
            .get(&number)
            .copied()
            .or_else(|| Some(keccak256(number.to_string().as_bytes())))
    }
}

/// A database implementation that stores initial state in memory.
pub type InMemoryDB = CacheDB;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x11; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(address, AccountInfo::default());
        database.insert_account_storage(address, Word::from(1), Word::from(10));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.set_storage(address, Word::from(1), Word::from(20));
        state.set_storage(address, Word::from(1), Word::from(30));

        assert_eq!(state.storage(address, Word::from(1)), Word::from(30));
        state.rollback(checkpoint);
        assert_eq!(state.storage(address, Word::from(1)), Word::from(10));
    }

    #[test]
    fn transient_storage_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x22; 20]);
        let mut state = State::new(CacheDB::default());

        state.set_transient_storage(address, Word::from(1), Word::from(10));
        let checkpoint = state.checkpoint();
        state.set_transient_storage(address, Word::from(1), Word::from(20));

        assert_eq!(state.transient_storage(address, Word::from(1)), Word::from(20));
        state.rollback(checkpoint);
        assert_eq!(state.transient_storage(address, Word::from(1)), Word::from(10));
    }

    #[test]
    fn destruct_change_rolls_back_to_checkpoint() {
        let address = Address::from([0x33; 20]);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        state.mark_destructed(address);

        assert!(state.is_selfdestructed(address));
        state.rollback(checkpoint);
        assert!(!state.is_selfdestructed(address));
    }

    #[test]
    fn spurious_dragon_deletes_touched_empty_existing_account() {
        let address = Address::from([0x44; 20]);
        let empty = AccountInfo::default();
        let mut database = CacheDB::default();
        database.insert_account_info(address, empty.clone());
        let mut state = State::new(database);

        state.touch(address);
        let changes = state.build_state_changes(crate::SpecId::SPURIOUS_DRAGON);

        let change = changes.accounts.get(&address).expect("touched empty account is deleted");
        assert_eq!(change.original, Some(empty));
        assert_eq!(change.current, None);
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
    }

    #[test]
    fn homestead_preserves_touched_empty_existing_account() {
        let address = Address::from([0x45; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(address, AccountInfo::default());
        let mut state = State::new(database);

        state.touch(address);
        let changes = state.build_state_changes(crate::SpecId::HOMESTEAD);

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn homestead_materializes_touched_empty_new_account() {
        let address = Address::from([0x46; 20]);
        let mut state = State::new(CacheDB::default());

        state.touch(address);
        let changes = state.build_state_changes(crate::SpecId::HOMESTEAD);

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

        state.touch(address);
        let changes = state.build_state_changes(crate::SpecId::SPURIOUS_DRAGON);

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn selfdestruct_deletes_account_and_wipes_storage() {
        let address = Address::from([0x48; 20]);
        let mut database = CacheDB::default();
        database.insert_account_info(address, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_storage(address, Word::from(1), Word::from(2));
        let mut state = State::new(database);

        state.mark_destructed(address);
        let changes = state.build_state_changes(crate::SpecId::SPURIOUS_DRAGON);

        let change = changes.accounts.get(&address).expect("selfdestruct deletes account");
        assert!(change.original.is_some());
        assert_eq!(change.current, None);
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
    }
}
