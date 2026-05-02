//! Basic in-memory EVM host state.

use crate::{bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{
    Address, B256, U256, keccak256,
    map::{self, HashMap},
};

/// Account information that contains balance, nonce, code hash, and code.
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
    ///
    /// If `None`, `code_hash` can be used to load bytecode from the contract cache.
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
        self.balance.is_zero()
            && self.nonce == 0
            && self.code.as_ref().is_none_or(Bytecode::is_empty)
    }
}

/// Persistent account state used by the basic EVM host.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct Account {
    /// Balance, nonce, and code.
    pub info: AccountInfo,
    /// Account status.
    pub status: AccountState,
}

impl Account {
    /// Creates a new non-existing account.
    #[inline]
    pub fn new_not_existing() -> Self {
        Self {
            info: AccountInfo::new(U256::ZERO, 0, KECCAK_EMPTY, Bytecode::new()),
            status: AccountState::NotExisting,
        }
    }

    /// Returns account info if this account exists.
    #[inline]
    pub fn info(&self) -> Option<AccountInfo> {
        if self.status == AccountState::NotExisting { None } else { Some(self.info.clone()) }
    }

    /// Returns whether this account is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.info.is_empty()
    }

    /// Returns whether this account is marked for self-destruction.
    #[inline]
    pub const fn is_selfdestructed(&self) -> bool {
        matches!(self.status, AccountState::SelfDestructed)
    }
}

/// State of an account in the database.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum AccountState {
    /// Before Spurious Dragon there was a difference between empty and non-existing accounts.
    NotExisting,
    /// EVM touched this account.
    Touched,
    /// EVM cleared storage of this account.
    StorageCleared,
    /// EVM marked this account for self-destruction.
    SelfDestructed,
    /// EVM did not interact with this account.
    #[default]
    None,
}

impl AccountState {
    /// Returns `true` if EVM cleared storage of this account.
    #[inline]
    pub const fn is_storage_cleared(self) -> bool {
        matches!(self, Self::StorageCleared | Self::SelfDestructed | Self::NotExisting)
    }
}

/// Storage slot with original, previous, and present values.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct StorageSlot {
    /// Value loaded from database before local execution changes.
    pub original_value: Word,
    /// Value before the latest write.
    pub previous_value: Word,
    /// Current value.
    pub present_value: Word,
}

impl StorageSlot {
    /// Creates a new unchanged storage slot.
    #[inline]
    pub const fn new(value: Word) -> Self {
        Self { original_value: value, previous_value: value, present_value: value }
    }

    /// Creates a new changed storage slot.
    #[inline]
    pub const fn new_changed(original_value: Word, present_value: Word) -> Self {
        Self { original_value, previous_value: original_value, present_value }
    }

    /// Returns whether the slot changed from its original value.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.original_value != self.present_value
    }

    /// Sets a new present value and stores the previous value.
    #[inline]
    pub const fn set(&mut self, value: Word) {
        self.previous_value = self.present_value;
        self.present_value = value;
    }
}

/// Database backing the basic EVM host.
///
/// TODO: Replace this with revm's full `Database`/`DatabaseRef` split and journal model. This
/// trait is intentionally small for now and does not represent account warming, storage warming,
/// snapshots, reverts, or database errors.
pub trait Database {
    /// Loads account information.
    fn basic(&mut self, address: Address) -> Option<AccountInfo>;

    /// Stores account information.
    fn insert_account_info(&mut self, address: Address, info: AccountInfo);

    /// Loads account code by hash.
    fn code_by_hash(&mut self, code_hash: B256) -> Bytecode;

    /// Loads a persistent storage slot.
    fn storage(&mut self, address: Address, key: Word) -> Word;

    /// Stores a persistent storage slot.
    fn set_storage(&mut self, address: Address, key: Word, value: Word);

    /// Clears persistent storage for an account.
    fn clear_storage(&mut self, address: Address);

    /// Returns whether an account was already self-destructed.
    fn is_selfdestructed(&mut self, address: Address) -> bool;

    /// Marks an account as self-destructed.
    fn mark_selfdestructed(&mut self, address: Address);

    /// Loads a historical block hash.
    fn block_hash(&mut self, number: u64) -> Option<B256>;
}

/// A cache used in [`CacheDB`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cache {
    /// Accounts keyed by address.
    pub accounts: HashMap<Address, Account>,
    /// Contracts keyed by code hash.
    pub contracts: HashMap<B256, Bytecode>,
    /// Persistent storage keyed by account and slot.
    pub storage: HashMap<(Address, Word), StorageSlot>,
    /// Cached block hashes keyed by block number.
    pub block_hashes: HashMap<u64, B256>,
}

impl Default for Cache {
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

/// A database implementation that stores all state changes in memory.
pub type InMemoryDB = CacheDB;

/// In-memory cache database.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheDB {
    /// The cache that stores state changes.
    pub cache: Cache,
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
            self.cache.contracts.entry(info.code_hash).or_insert_with(|| code.clone());
        }
        if info.code_hash.is_zero() {
            info.code_hash = KECCAK_EMPTY;
        }
    }

    /// Inserts account info without overriding storage.
    #[inline]
    pub fn insert_account_info(&mut self, address: Address, mut info: AccountInfo) {
        self.insert_contract(&mut info);
        let account = self.cache.accounts.entry(address).or_default();
        account.info = info;
        if account.status == AccountState::NotExisting {
            account.status = AccountState::None;
        }
    }

    /// Returns account info if the account exists.
    #[inline]
    pub fn account_info(&self, address: Address) -> Option<&AccountInfo> {
        self.cache.accounts.get(&address).and_then(|account| {
            (account.status != AccountState::NotExisting).then_some(&account.info)
        })
    }

    /// Returns an account if present in the cache.
    #[inline]
    pub fn account_ref(&self, address: Address) -> Option<&Account> {
        self.cache.accounts.get(&address)
    }

    /// Inserts persistent storage while preserving the original value.
    #[inline]
    pub fn insert_account_storage(&mut self, address: Address, key: Word, value: Word) {
        self.cache.storage.insert((address, key), StorageSlot::new(value));
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.cache.block_hashes.insert(number, hash);
    }
}

impl Database for CacheDB {
    #[inline]
    fn basic(&mut self, address: Address) -> Option<AccountInfo> {
        self.cache.accounts.get(&address).and_then(Account::info)
    }

    #[inline]
    fn insert_account_info(&mut self, address: Address, info: AccountInfo) {
        Self::insert_account_info(self, address, info);
    }

    #[inline]
    fn code_by_hash(&mut self, code_hash: B256) -> Bytecode {
        self.cache.contracts.get(&code_hash).cloned().unwrap_or_default()
    }

    #[inline]
    fn storage(&mut self, address: Address, key: Word) -> Word {
        if self
            .cache
            .accounts
            .get(&address)
            .is_some_and(|account| account.status.is_storage_cleared())
        {
            return Word::ZERO;
        }
        self.cache.storage.get(&(address, key)).map_or(Word::ZERO, |slot| slot.present_value)
    }

    #[inline]
    fn set_storage(&mut self, address: Address, key: Word, value: Word) {
        let slot = self.cache.storage.entry((address, key)).or_default();
        slot.set(value);
    }

    #[inline]
    fn clear_storage(&mut self, address: Address) {
        for ((slot_address, _), slot) in &mut self.cache.storage {
            if *slot_address == address {
                slot.set(Word::ZERO);
            }
        }
        self.cache.accounts.entry(address).or_default().status = AccountState::StorageCleared;
    }

    #[inline]
    fn is_selfdestructed(&mut self, address: Address) -> bool {
        self.cache.accounts.get(&address).is_some_and(Account::is_selfdestructed)
    }

    #[inline]
    fn mark_selfdestructed(&mut self, address: Address) {
        self.clear_storage(address);
        self.cache.accounts.entry(address).or_default().status = AccountState::SelfDestructed;
    }

    #[inline]
    fn block_hash(&mut self, number: u64) -> Option<B256> {
        self.cache
            .block_hashes
            .get(&number)
            .copied()
            .or_else(|| Some(keccak256(number.to_string().as_bytes())))
    }
}

/// Hash of the empty bytecode.
pub const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0, 0xe5,
    0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70, 0x00,
]);
