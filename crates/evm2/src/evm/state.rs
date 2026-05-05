//! Basic in-memory EVM host state.

use super::{SStore, db::Database};
use crate::{
    SpecId,
    bytecode::Bytecode,
    interpreter::{InstrStop, Word},
};
use alloc::{collections::BTreeMap, vec::Vec};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY, Log, U256,
    map::{AddressMap, AddressSet, HashMap, HashSet, U256Map, hash_map},
};

/// A value tracked together with the value it had at the start of the current
/// transaction.
///
/// `Tracked` is used by [`State`] to keep an overlay over the backing database
/// and by [`StateChanges`] to describe account and storage transitions.
/// `original` is the value at the current transaction boundary, while `current`
/// is the value after all in-flight EVM mutations. When a transaction is
/// accepted, `current` becomes the next transaction's `original` without writing
/// anything to the backing database.
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
            code_hash: KECCAK256_EMPTY,
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
        self.balance.is_zero() && self.nonce == 0 && self.code_hash == KECCAK256_EMPTY
    }
}

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
        self.nonce == 0 && self.balance.is_zero() && self.code_hash == KECCAK256_EMPTY
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
    pub(crate) slots: U256Map<Tracked<Word>>,
}

/// Complete state transition and emitted logs produced by a transaction.
///
/// `StateChanges` is the public write-set returned in [`crate::TxResult`]. It
/// is intentionally explicit so embedding clients can update their own database
/// and compute post-state roots without reimplementing EVM account-lifetime
/// rules. It also carries the logs emitted by the transaction.
///
/// Consumers should apply database changes in this order:
///
/// 1. write bytecode from [`Self::code`] for every non-empty code hash they do not already have;
/// 2. for each [`StorageChangeSet`] whose [`StorageChangeSet::wipe`] flag is true, delete all
///    storage for that account;
/// 3. apply each storage slot change: a zero [`Tracked::current`] means delete the slot, otherwise
///    write the slot value;
/// 4. apply account changes: `current = Some(..)` means upsert the account, `current = None` means
///    delete the account.
///
/// `evm2` does not write to the backing database. These changes describe what
/// happened; applying them is the responsibility of the caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateChanges {
    /// Account changes keyed by address.
    ///
    /// [`Tracked::original`] is the account at the beginning of the transaction.
    /// [`Tracked::current`] is the account after transaction execution and EVM
    /// account-lifetime rules have been evaluated. `current = None` is an explicit account
    /// deletion.
    pub accounts: BTreeMap<Address, Tracked<Option<AccountInfo>>>,
    /// Persistent storage changes keyed by account address.
    ///
    /// Each slot change's [`Tracked::original`] value is the slot value at the beginning of the
    /// transaction, after any storage wipe/re-incarnation semantics that occurred before the slot
    /// was loaded. `current = 0` means the consumer should delete the slot.
    pub storage: BTreeMap<Address, StorageChangeSet>,
    /// Newly created or modified bytecode keyed by code hash.
    pub code: BTreeMap<B256, Bytecode>,
    /// Logs emitted by the transaction.
    pub logs: Vec<Log>,
}

impl StateChanges {
    /// Returns whether this transition contains no changes or logs.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
            && self.storage.is_empty()
            && self.code.is_empty()
            && self.logs.is_empty()
    }
}

/// Storage transition for a single account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageChangeSet {
    /// If true, delete all pre-existing storage for this account before applying
    /// [`Self::slots`]. This is used for selfdestruct and contract
    /// re-incarnation semantics using an explicit storage wipe marker.
    pub wipe: bool,
    /// Changed storage slots keyed by slot.
    pub slots: BTreeMap<Word, Tracked<Word>>,
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
    /// Log was emitted.
    Log {
        /// Log index before emission.
        index: usize,
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
    pub accounts: AddressMap<Tracked<Option<Account>>>,
    /// Persistent storage overlay keyed by account address.
    pub storage: AddressMap<StorageOverlay>,
    /// Revert journal.
    pub journal: Vec<JournalEntry>,
    /// Logs emitted by the current transaction.
    pub logs: Vec<Log>,
    /// Accounts touched for transaction-finalization account-lifetime rules.
    ///
    /// This is separate from the account overlay and the EIP-2929 warm set. A
    /// touched account may have no field changes, but can still matter for empty
    /// account deletion/materialization rules across forks.
    pub touched: AddressSet,
    /// Accounts self-destructed in the current transaction.
    pub selfdestructs: AddressSet,
    /// Transaction-scoped warm account set for EIP-2929 gas accounting.
    ///
    /// This tracks whether account access is warm or cold. It does not imply the
    /// account was touched, changed, or should be emitted in [`StateChanges`].
    pub accessed_accounts: AddressSet,
    /// Transaction-scoped warm storage slot set.
    pub accessed_storage: HashSet<(Address, Word)>,
    /// Transaction-scoped EIP-1153 transient storage keyed by account address and slot.
    pub transient_storage: HashMap<(Address, Word), Word>,
}

impl<D> State<D> {
    /// Creates a new state over an initial database.
    #[inline]
    pub fn new(initial: D) -> Self {
        Self {
            initial,
            accounts: AddressMap::default(),
            storage: AddressMap::default(),
            journal: Vec::new(),
            logs: Vec::new(),
            touched: AddressSet::default(),
            selfdestructs: AddressSet::default(),
            accessed_accounts: AddressSet::default(),
            accessed_storage: HashSet::default(),
            transient_storage: HashMap::default(),
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

    /// Returns logs emitted by the current in-flight transaction.
    #[inline]
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Records a transaction log and journals it for rollback.
    #[inline]
    pub fn log(&mut self, log: Log) {
        let index = self.logs.len();
        self.journal.push(JournalEntry::Log { index });
        self.logs.push(log);
    }

    /// Returns the current account overlay if present and not deleted.
    #[inline]
    #[must_use]
    pub fn account_ref(&self, address: Address) -> Option<&Account> {
        self.accounts.get(&address)?.current.as_ref()
    }

    /// Returns whether an account is warm in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_account_warm(&self, address: Address) -> bool {
        self.accessed_accounts.contains(&address)
    }

    /// Marks an account as warm.
    #[inline]
    pub fn warm_account(&mut self, address: Address) {
        if self.accessed_accounts.insert(address) {
            self.journal.push(JournalEntry::AccountWarmed { address });
        }
    }

    /// Marks accounts as warm.
    #[inline]
    pub fn warm_accounts(&mut self, addresses: impl IntoIterator<Item = Address>) {
        let addresses = addresses.into_iter();
        self.accessed_accounts.reserve(addresses.size_hint().0);
        for address in addresses {
            self.warm_account(address);
        }
    }

    /// Returns whether a storage slot is warm in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_storage_warm(&self, address: Address, key: Word) -> bool {
        self.accessed_storage.contains(&(address, key))
    }

    /// Marks a storage slot as warm and returns whether it was cold before this access.
    #[inline]
    #[must_use]
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
        self.transient_storage.clear();
        self.logs.clear();
    }
}

impl<D: Database> State<D> {
    #[must_use]
    fn load_account(&mut self, address: Address) -> Option<&mut Tracked<Option<Account>>> {
        match self.accounts.entry(address) {
            hash_map::Entry::Occupied(entry) => Some(entry.into_mut()),
            hash_map::Entry::Vacant(entry) => {
                let info = self.initial.get_account(address)?;
                Some(entry.insert(Tracked::new(Some(Account::from_info(info)))))
            }
        }
    }

    #[must_use]
    fn ensure_account_overlay<'a>(
        initial: &mut D,
        accounts: &'a mut AddressMap<Tracked<Option<Account>>>,
        journal: &mut Vec<JournalEntry>,
        address: Address,
    ) -> &'a mut Tracked<Option<Account>> {
        match accounts.entry(address) {
            hash_map::Entry::Occupied(entry) => entry.into_mut(),
            hash_map::Entry::Vacant(entry) => {
                let original = initial.get_account(address).map(Account::from_info);
                journal.push(JournalEntry::AccountInserted { address });
                entry.insert(Tracked { original: original.clone(), current: original })
            }
        }
    }

    #[must_use]
    fn account_mut(&mut self, address: Address) -> &mut Account {
        let tracked = Self::ensure_account_overlay(
            &mut self.initial,
            &mut self.accounts,
            &mut self.journal,
            address,
        );
        if tracked.current.is_none() {
            self.journal.push(JournalEntry::AccountChange { address, previous: None });
        }
        tracked.current.get_or_insert_with(|| Account {
            code_hash: KECCAK256_EMPTY,
            code: Bytecode::default(),
            ..Account::default()
        })
    }

    #[must_use]
    fn journal_account_change(&mut self, address: Address) -> &mut Account {
        let tracked = Self::ensure_account_overlay(
            &mut self.initial,
            &mut self.accounts,
            &mut self.journal,
            address,
        );
        let previous = tracked.current.clone();
        self.journal.push(JournalEntry::AccountChange { address, previous });
        tracked.current.get_or_insert_with(|| Account {
            code_hash: KECCAK256_EMPTY,
            code: Bytecode::default(),
            ..Account::default()
        })
    }

    /// Returns account info.
    #[inline]
    #[must_use]
    pub fn account_info(&mut self, address: Address) -> Option<AccountInfo> {
        if let Some(account) = self.accounts.get(&address) {
            return account.current.as_ref().map(Account::info);
        }
        self.initial.get_account(address)
    }

    /// Returns an account if it exists.
    #[inline]
    #[must_use]
    pub fn find(&mut self, address: Address) -> Option<&Account> {
        self.load_account(address)?.current.as_ref()
    }

    /// Gets an existing account or inserts a new empty account.
    #[inline]
    pub fn get_or_insert(&mut self, address: Address) -> &mut Account {
        self.account_mut(address)
    }

    /// Gets account code.
    #[inline]
    #[must_use]
    pub fn get_code(&mut self, address: Address) -> Bytecode {
        let Some(account) = self.find(address) else {
            return Bytecode::default();
        };
        if account.code_hash == KECCAK256_EMPTY {
            return Bytecode::default();
        }
        if !account.code.is_empty() {
            return account.code.clone();
        }
        self.initial.get_account_code(address)
    }

    #[must_use]
    fn storage_initial(&mut self, address: Address, key: Word) -> Word {
        if self.storage.get(&address).is_some_and(|storage| storage.wiped)
            || self.accounts.get(&address).is_some_and(|account| account.original.is_none())
        {
            Word::ZERO
        } else {
            self.initial.get_storage(address, key)
        }
    }

    #[must_use]
    fn storage_slot_mut(
        &mut self,
        address: Address,
        key: Word,
        journal_insert: bool,
    ) -> &mut Tracked<Word> {
        let initial = self.storage_initial(address, key);
        let storage = self.storage.entry(address).or_default();
        match storage.slots.entry(key) {
            hash_map::Entry::Occupied(entry) => entry.into_mut(),
            hash_map::Entry::Vacant(entry) => {
                if journal_insert {
                    self.journal.push(JournalEntry::StorageInserted { address, key });
                }
                entry.insert(Tracked::new(initial))
            }
        }
    }

    /// Loads persistent storage.
    #[inline]
    #[must_use]
    pub fn storage(&mut self, address: Address, key: Word) -> Word {
        let Some(_) = self.account_info(address) else {
            return Word::ZERO;
        };
        self.storage_slot_mut(address, key, false).current
    }

    /// Stores persistent storage and returns values needed for `SSTORE` gas metering.
    ///
    /// This is a raw state mutation helper, not the full EVM `SSTORE` host operation. It does
    /// not perform static-call checks, gas/stipend checks, EIP-2929 cold-access handling, refund
    /// accounting, or Amsterdam state-gas charging. Instruction implementations should call the
    /// host `sstore` operation instead, and only use this lower-level helper when those concerns
    /// are handled elsewhere.
    #[inline]
    pub fn set_storage(&mut self, address: Address, key: Word, value: Word) -> SStore {
        let _ = self.account_mut(address);
        self.touch(address);
        let slot = self.storage_slot_mut(address, key, true);
        let result = SStore {
            original_value: slot.original,
            present_value: slot.current,
            new_value: value,
            is_cold: false,
        };
        if slot.current != value {
            let previous = slot.current;
            slot.current = value;
            self.journal.push(JournalEntry::StorageChange { address, key, previous });
        }
        result
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
        let account = self.journal_account_change(address);
        account.balance = account.balance.wrapping_add(delta);
        self.touch(address);
    }

    /// Transfers value between accounts.
    #[inline]
    #[must_use]
    pub fn transfer(&mut self, from: Address, to: Address, value: Word) -> bool {
        if value.is_zero() || from == to {
            self.touch(to);
            return true;
        }

        let from_balance = self.account_info(from).map_or(Word::ZERO, |info| info.balance);
        let Some(new_from_balance) = from_balance.checked_sub(value) else {
            return false;
        };

        self.journal_account_change(from).balance = new_from_balance;
        self.touch(from);

        let account = self.journal_account_change(to);
        account.balance = account.balance.saturating_add(value);
        self.touch(to);
        true
    }

    /// Increments account nonce.
    #[inline]
    pub fn increment_nonce(&mut self, address: Address) {
        let account = self.journal_account_change(address);
        account.nonce = account.nonce.saturating_add(1);
        self.touch(address);
    }

    /// Creates a contract account and transfers endowment from the caller.
    #[inline(never)]
    pub fn create_account(
        &mut self,
        caller: Address,
        address: Address,
        value: Word,
        spec: SpecId,
    ) -> Result<(), InstrStop> {
        if let Some(info) = self.account_info(address)
            && (info.nonce != 0 || info.code_hash != KECCAK256_EMPTY)
        {
            return Err(InstrStop::CreateCollision);
        }

        if !self.transfer(caller, address, value) {
            return Err(InstrStop::OutOfFunds);
        }

        let balance = self.account_mut(address).balance;
        self.wipe_storage(address);
        let account = self.journal_account_change(address);
        *account = Account {
            nonce: u64::from(spec.enables(SpecId::SPURIOUS_DRAGON)),
            balance,
            code_hash: KECCAK256_EMPTY,
            code: Bytecode::default(),
            just_created: true,
            code_changed: true,
        };
        self.touch(address);
        Ok(())
    }

    /// Sets account bytecode.
    #[inline]
    pub fn set_code(&mut self, address: Address, code: Bytecode) {
        let account = self.journal_account_change(address);
        account.code_hash = code.hash_slow();
        account.code = code;
        account.code_changed = true;
    }

    /// Marks all prior persistent storage for `address` as deleted.
    #[inline]
    pub fn wipe_storage(&mut self, address: Address) {
        let previous = self.storage.get(&address).cloned();
        self.journal.push(JournalEntry::StorageWipe { address, previous });
        self.storage.insert(address, StorageOverlay { wiped: true, slots: U256Map::default() });
    }

    /// Loads transient storage.
    #[inline]
    #[must_use]
    pub fn transient_storage(&mut self, address: Address, key: Word) -> Word {
        self.transient_storage.get(&(address, key)).copied().unwrap_or_default()
    }

    /// Stores transient storage.
    #[inline]
    pub fn set_transient_storage(&mut self, address: Address, key: Word, value: Word) {
        match self.transient_storage.entry((address, key)) {
            hash_map::Entry::Occupied(mut entry) => {
                let previous = *entry.get();
                if previous == value {
                    return;
                }
                self.journal.push(JournalEntry::TransientStorageChange {
                    address,
                    key,
                    previous: Some(previous),
                });
                if value.is_zero() {
                    entry.remove();
                } else {
                    *entry.get_mut() = value;
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if value.is_zero() {
                    return;
                }
                self.journal.push(JournalEntry::TransientStorageChange {
                    address,
                    key,
                    previous: None,
                });
                entry.insert(value);
            }
        }
    }

    /// Marks an account as self-destructed in the current transaction.
    #[inline]
    pub fn mark_destructed(&mut self, address: Address) {
        let _ = Self::ensure_account_overlay(
            &mut self.initial,
            &mut self.accounts,
            &mut self.journal,
            address,
        );
        if self.selfdestructs.insert(address) {
            self.journal.push(JournalEntry::SelfDestruct { address });
        }
        self.touch(address);
    }

    /// Returns whether an account has been marked self-destructed in the current transaction.
    #[inline]
    #[must_use]
    pub fn is_selfdestructed(&self, address: Address) -> bool {
        self.selfdestructs.contains(&address)
    }

    /// Returns whether an account was created in the current transaction.
    #[inline]
    #[must_use]
    pub(super) fn is_created_in_transaction(&self, address: Address) -> bool {
        self.account_ref(address).is_some_and(|account| account.just_created)
    }

    /// Reverts state changes after the checkpoint.
    #[inline(never)]
    pub fn rollback(&mut self, checkpoint: usize) {
        assert!(checkpoint <= self.journal.len(), "checkpoint is past journal length");
        while self.journal.len() != checkpoint {
            let Some(entry) = self.journal.pop() else {
                unreachable!("checkpoint is checked above")
            };
            match entry {
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
                JournalEntry::TransientStorageChange { address, key, previous } => match previous {
                    Some(previous) if !previous.is_zero() => {
                        self.transient_storage.insert((address, key), previous);
                    }
                    _ => {
                        self.transient_storage.remove(&(address, key));
                    }
                },
                JournalEntry::AccountWarmed { address } => {
                    self.accessed_accounts.remove(&address);
                }
                JournalEntry::StorageWarmed { address, key } => {
                    self.accessed_storage.remove(&(address, key));
                }
                JournalEntry::Log { index } => {
                    self.logs.truncate(index);
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
    #[must_use]
    fn is_existing_dead(&mut self, address: Address) -> bool {
        if let Some(account) = self.accounts.get(&address) {
            return account.current.as_ref().is_some_and(Account::is_empty)
                || (account.current.is_none() && account.original.is_some());
        }
        self.initial.get_account(address).is_some_and(|account| account.is_empty())
    }

    fn delete_account_for_finalization(&mut self, address: Address) {
        let account = Self::ensure_account_overlay(
            &mut self.initial,
            &mut self.accounts,
            &mut self.journal,
            address,
        );
        account.current = None;
        self.storage.insert(address, StorageOverlay { wiped: true, slots: U256Map::default() });
    }

    fn materialize_empty_account_for_finalization(&mut self, address: Address) {
        let account = Self::ensure_account_overlay(
            &mut self.initial,
            &mut self.accounts,
            &mut self.journal,
            address,
        );
        if account.original.is_none() {
            account.current.get_or_insert_with(|| Account {
                code_hash: KECCAK256_EMPTY,
                code: Bytecode::default(),
                ..Account::default()
            });
        }
    }

    /// Applies transaction-finalization account-lifetime rules to the overlay.
    ///
    /// This mutates the in-memory post-transaction state before it is serialized
    /// by [`Self::build_state_changes`]. Runtime records
    /// transaction substate such as touches and selfdestructs, while finalization
    /// turns that substate into account deletions, storage wipes, or pre-EIP-161
    /// empty-account materialization.
    pub(super) fn finalize_transaction(&mut self, spec: SpecId) {
        let selfdestructs = core::mem::take(&mut self.selfdestructs);
        for address in selfdestructs.iter().copied() {
            self.delete_account_for_finalization(address);
        }

        let touched = core::mem::take(&mut self.touched);
        if spec.enables(SpecId::SPURIOUS_DRAGON) {
            for address in touched {
                // EIP-161 deletes touched dead accounts at transaction finalization.
                if self.is_existing_dead(address) {
                    self.delete_account_for_finalization(address);
                }
            }
        } else {
            for address in touched {
                // Before EIP-161, touching a non-existent account materializes it as empty.
                if !selfdestructs.contains(&address) && self.account_info(address).is_none() {
                    self.materialize_empty_account_for_finalization(address);
                }
            }
        }
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

        for (&address, tracked) in &self.accounts {
            let original = tracked.original.as_ref().map(Account::info);
            let current = tracked.current.as_ref().map(Account::info);
            if original != current {
                changes.accounts.insert(address, Tracked { original, current });
            }
            if let Some(account) = tracked.current.as_ref() {
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
            let mut set = StorageChangeSet { wipe: storage.wiped, slots: BTreeMap::new() };
            for (&key, slot) in &storage.slots {
                if slot.original != slot.current && (!set.wipe || !slot.current.is_zero()) {
                    set.slots
                        .insert(key, Tracked { original: slot.original, current: slot.current });
                }
            }
            if set.wipe || !set.slots.is_empty() {
                changes.storage.insert(address, set);
            }
        }

        changes
    }

    /// Marks the current transaction's overlay values as the new baseline.
    ///
    /// After [`Self::finalize_transaction`] and [`Self::build_state_changes`] have
    /// produced the transaction write-set, the overlay must stop treating those
    /// writes as pending changes. This rolls the current account and storage
    /// values forward into their `original` slots and applies local deletion/wipe
    /// bookkeeping. It only advances the in-memory overlay; callers are still
    /// responsible for applying the emitted write-set to their backing database
    /// and clearing transaction-local state.
    pub(super) fn commit_transaction_overlay(&mut self) {
        for account in self.accounts.values_mut() {
            if let Some(current) = &mut account.current {
                current.just_created = false;
                current.code_changed = false;
            }
            account.original.clone_from(&account.current);
        }

        self.storage.retain(|_, storage| {
            if storage.wiped {
                storage.slots.retain(|_, slot| !slot.current.is_zero());
            }
            for slot in storage.slots.values_mut() {
                slot.original = slot.current;
            }
            storage.wiped = false;
            !storage.slots.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::CacheDB;

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
        state.rollback(checkpoint);
        assert_eq!(state.logs(), &[kept]);
    }

    #[test]
    fn state_changes_take_logs_from_transaction_state() {
        use alloy_primitives::{Bytes, LogData};

        let mut state = State::new(CacheDB::default());
        let log = Log {
            address: Address::from([0x66; 20]),
            data: LogData::new_unchecked(Vec::new(), Bytes::from_static(&[0x03])),
        };

        state.log(log.clone());
        state.finalize_transaction(crate::SpecId::SPURIOUS_DRAGON);
        let changes = state.build_state_changes();
        assert_eq!(changes.logs.as_slice(), core::slice::from_ref(&log));
        assert!(state.logs().is_empty());

        state.commit_transaction_overlay();
        state.clear_transaction_state();
        assert!(state.logs().is_empty());
    }

    #[test]
    fn spurious_dragon_deletes_touched_empty_existing_account() {
        let address = Address::from([0x44; 20]);
        let empty = AccountInfo::default();
        let mut database = CacheDB::default();
        database.insert_account_info(address, empty.clone());
        let mut state = State::new(database);

        state.touch(address);
        state.finalize_transaction(crate::SpecId::SPURIOUS_DRAGON);
        let changes = state.build_state_changes();

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
        state.finalize_transaction(crate::SpecId::HOMESTEAD);
        let changes = state.build_state_changes();

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
    }

    #[test]
    fn homestead_materializes_touched_empty_new_account() {
        let address = Address::from([0x46; 20]);
        let mut state = State::new(CacheDB::default());

        state.touch(address);
        state.finalize_transaction(crate::SpecId::HOMESTEAD);
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

        state.touch(address);
        state.finalize_transaction(crate::SpecId::SPURIOUS_DRAGON);
        let changes = state.build_state_changes();

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
        state.finalize_transaction(crate::SpecId::SPURIOUS_DRAGON);
        let changes = state.build_state_changes();

        let change = changes.accounts.get(&address).expect("selfdestruct deletes account");
        assert!(change.original.is_some());
        assert_eq!(change.current, None);
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
    }
}
