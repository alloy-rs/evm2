//! Basic in-memory EVM host state.

use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{string::ToString, vec::Vec};
use alloy_primitives::{
    Address, B256, U256, keccak256,
    map::{self, HashMap, HashSet},
};

pub use alloy_primitives::KECCAK256_EMPTY as KECCAK_EMPTY;

/// Account information loaded from the backing database.
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
    /// Whether the account has initial persistent storage.
    pub has_storage: bool,
}

impl Default for AccountInfo {
    #[inline]
    fn default() -> Self {
        Self {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            code: Some(Bytecode::default()),
            has_storage: false,
        }
    }
}

impl AccountInfo {
    /// Creates a new [`AccountInfo`] with the given fields.
    #[inline]
    pub const fn new(balance: Word, nonce: u64, code_hash: B256, code: Bytecode) -> Self {
        Self { balance, nonce, code_hash, code: Some(code), has_storage: false }
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

/// Persistent storage value cached by [`State`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct StorageValue {
    /// Current value.
    pub current: Word,
    /// Original value loaded from the backing database.
    pub original: Word,
}

impl StorageValue {
    /// Creates a new unchanged storage value.
    #[inline]
    pub const fn new(value: Word) -> Self {
        Self { current: value, original: value }
    }

    /// Returns whether the value changed from its original value.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.current != self.original
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
    /// Whether the account has initial persistent storage.
    pub has_initial_storage: bool,
    /// Cached and modified persistent storage entries.
    pub storage: HashMap<Word, StorageValue>,
    /// EIP-1153 transient storage.
    pub transient_storage: HashMap<Word, Word>,
    /// Cached account bytecode.
    pub code: Bytecode,
    /// Whether the account has been self-destructed.
    pub destructed: bool,
    /// Whether the account should be deleted if empty.
    pub erase_if_empty: bool,
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
            has_initial_storage: info.has_storage,
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
            has_storage: self.has_initial_storage
                || self.storage.values().any(StorageValue::is_changed),
        }
    }

    /// Returns whether this account is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nonce == 0 && self.balance.is_zero() && self.code_hash == KECCAK_EMPTY
    }
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
    /// Account balance changed.
    BalanceChange {
        /// Account address.
        address: Address,
        /// Previous balance.
        previous: Word,
    },
    /// Account was touched.
    Touched {
        /// Account address.
        address: Address,
    },
    /// Persistent storage changed.
    StorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous storage value.
        previous: Word,
    },
    /// Transient storage changed.
    TransientStorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous transient storage value.
        previous: Word,
    },
    /// Account nonce was incremented.
    NonceBump {
        /// Account address.
        address: Address,
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
    /// Account was created.
    Create {
        /// Account address.
        address: Address,
        /// Whether the account existed before creation.
        existed: bool,
    },
    /// Account was self-destructed.
    Destruct {
        /// Account address.
        address: Address,
    },
}

/// Mutable EVM state with evmone-style journaling.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct State<D> {
    /// Read-only initial database.
    pub initial: D,
    /// Accounts loaded from the initial database and potentially modified.
    pub modified: HashMap<Address, Account>,
    /// Revert journal.
    pub journal: Vec<JournalEntry>,
    /// Transaction-scoped warm account set.
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
            modified: map::HashMap::default(),
            journal: Vec::new(),
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

    /// Returns the modified account if present.
    #[inline]
    pub fn account_ref(&self, address: Address) -> Option<&Account> {
        self.modified.get(&address)
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

    /// Clears all transaction-scoped warm accesses.
    #[inline]
    pub fn clear_accesses(&mut self) {
        self.accessed_accounts.clear();
        self.accessed_storage.clear();
    }
}

impl<D: Database> State<D> {
    /// Returns account info.
    #[inline]
    pub fn account_info(&self, address: Address) -> Option<AccountInfo> {
        self.modified.get(&address).map(Account::info).or_else(|| self.initial.get_account(address))
    }

    /// Returns an account if it exists.
    #[inline]
    pub fn find(&mut self, address: Address) -> Option<&Account> {
        if !self.modified.contains_key(&address)
            && let Some(info) = self.initial.get_account(address)
        {
            self.modified.insert(address, Account::from_info(info));
        }
        self.modified.get(&address)
    }

    /// Gets an existing account or inserts a new empty account.
    #[inline]
    pub fn get_or_insert(&mut self, address: Address) -> &mut Account {
        if !self.modified.contains_key(&address) {
            let account =
                self.initial.get_account(address).map(Account::from_info).unwrap_or_else(|| {
                    Account {
                        code_hash: KECCAK_EMPTY,
                        code: Bytecode::default(),
                        ..Account::default()
                    }
                });
            self.modified.insert(address, account);
        }
        self.modified.get_mut(&address).expect("account was just inserted")
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

    /// Gets a persistent storage value.
    #[inline]
    pub fn get_storage_value(&mut self, address: Address, key: Word) -> &mut StorageValue {
        if !self.get_or_insert(address).storage.contains_key(&key) {
            let initial = self.initial.get_storage(address, key);
            self.get_or_insert(address).storage.insert(key, StorageValue::new(initial));
        }
        self.get_or_insert(address).storage.get_mut(&key).expect("storage was just inserted")
    }

    /// Loads persistent storage.
    #[inline]
    pub fn storage(&mut self, address: Address, key: Word) -> Word {
        self.get_storage_value(address, key).current
    }

    /// Stores persistent storage.
    #[inline]
    pub fn set_storage(&mut self, address: Address, key: Word, value: Word) {
        let previous = self.get_storage_value(address, key).current;
        self.journal.push(JournalEntry::StorageChange { address, key, previous });
        self.get_storage_value(address, key).current = value;
    }

    /// Adds a signed balance delta by wrapping two's-complement values.
    #[inline]
    pub fn add_balance(&mut self, address: Address, delta: Word) {
        if delta.is_zero() {
            self.get_or_insert(address).erase_if_empty = true;
            return;
        }
        let previous = self.get_or_insert(address).balance;
        self.journal.push(JournalEntry::BalanceChange { address, previous });
        self.get_or_insert(address).balance = previous.wrapping_add(delta);
    }

    /// Transfers value between accounts.
    #[inline]
    pub fn transfer(&mut self, from: Address, to: Address, value: Word) -> bool {
        if value.is_zero() || from == to {
            self.get_or_insert(to).erase_if_empty = true;
            return true;
        }

        let from_balance = self.account_info(from).map_or(Word::ZERO, |info| info.balance);
        let Some(new_from_balance) = from_balance.checked_sub(value) else {
            return false;
        };

        let from_previous = self.get_or_insert(from).balance;
        self.journal.push(JournalEntry::BalanceChange { address: from, previous: from_previous });
        self.get_or_insert(from).balance = new_from_balance;

        let to_previous = self.get_or_insert(to).balance;
        self.journal.push(JournalEntry::BalanceChange { address: to, previous: to_previous });
        self.get_or_insert(to).balance = to_previous.saturating_add(value);
        true
    }

    /// Increments account nonce.
    #[inline]
    pub fn increment_nonce(&mut self, address: Address) {
        self.journal.push(JournalEntry::NonceBump { address });
        self.get_or_insert(address).nonce = self.get_or_insert(address).nonce.saturating_add(1);
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
        let existed = self.account_info(address).is_some();
        if let Some(info) = self.account_info(address)
            && (info.nonce != 0 || info.code_hash != KECCAK_EMPTY)
        {
            return Err(crate::interpreter::InstrStop::CreateCollision);
        }

        self.journal.push(JournalEntry::Create { address, existed });
        if !self.transfer(caller, address, value) {
            return Err(crate::interpreter::InstrStop::OutOfFunds);
        }

        let account = self.get_or_insert(address);
        account.nonce = u64::from(spec.enables(crate::SpecId::SPURIOUS_DRAGON));
        account.code_hash = KECCAK_EMPTY;
        account.code = Bytecode::default();
        account.storage.clear();
        account.transient_storage.clear();
        account.destructed = false;
        account.just_created = true;
        account.code_changed = true;
        Ok(())
    }

    /// Sets account bytecode.
    #[inline]
    pub fn set_code(&mut self, address: Address, code: Bytecode) {
        let account = self.get_or_insert(address);
        account.code_hash = code.hash_slow();
        account.code = code;
        account.code_changed = true;
    }

    /// Removes modified accounts that should be erased and are empty.
    #[inline]
    pub fn prune_empty_accounts(&mut self) {
        self.modified.retain(|_, account| {
            !(account.erase_if_empty && account.is_empty() && !account.has_initial_storage)
        });
    }

    /// Loads transient storage.
    #[inline]
    pub fn transient_storage(&mut self, address: Address, key: Word) -> Word {
        self.get_or_insert(address).transient_storage.get(&key).copied().unwrap_or_default()
    }

    /// Stores transient storage.
    #[inline]
    pub fn set_transient_storage(&mut self, address: Address, key: Word, value: Word) {
        let previous = self.transient_storage(address, key);
        self.journal.push(JournalEntry::TransientStorageChange { address, key, previous });
        self.get_or_insert(address).transient_storage.insert(key, value);
    }

    /// Marks an account as self-destructed.
    #[inline]
    pub fn mark_destructed(&mut self, address: Address) {
        self.journal.push(JournalEntry::Destruct { address });
        self.get_or_insert(address).destructed = true;
    }

    /// Reverts state changes after the checkpoint.
    #[inline]
    pub fn rollback(&mut self, checkpoint: usize) {
        while self.journal.len() != checkpoint {
            match self.journal.pop().expect("journal length checked above") {
                JournalEntry::BalanceChange { address, previous } => {
                    self.get_or_insert(address).balance = previous;
                }
                JournalEntry::Touched { address } => {
                    self.get_or_insert(address).erase_if_empty = false;
                }
                JournalEntry::StorageChange { address, key, previous } => {
                    self.get_storage_value(address, key).current = previous;
                }
                JournalEntry::TransientStorageChange { address, key, previous } => {
                    self.get_or_insert(address).transient_storage.insert(key, previous);
                }
                JournalEntry::NonceBump { address } => {
                    self.get_or_insert(address).nonce -= 1;
                }
                JournalEntry::AccountWarmed { address } => {
                    self.accessed_accounts.remove(&address);
                }
                JournalEntry::StorageWarmed { address, key } => {
                    self.accessed_storage.remove(&(address, key));
                }
                JournalEntry::Create { address, existed } => {
                    if existed {
                        let account = self.get_or_insert(address);
                        account.nonce = 0;
                        account.code_hash = KECCAK_EMPTY;
                        account.code = Bytecode::default();
                    } else {
                        self.modified.remove(&address);
                    }
                }
                JournalEntry::Destruct { address } => {
                    self.get_or_insert(address).destructed = false;
                }
            }
        }
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
        self.accounts.entry(address).or_default().has_storage = true;
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

        assert!(state.account_ref(address).unwrap().destructed);
        state.rollback(checkpoint);
        assert!(!state.account_ref(address).unwrap().destructed);
    }
}
