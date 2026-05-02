//! Basic in-memory EVM host state.

use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{collections::BTreeMap, vec::Vec};
use alloy_primitives::{Address, B256, Log, U256};

/// Persistent account state used by the basic EVM host.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Account {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Account code hash.
    pub code_hash: B256,
    /// Account bytecode.
    pub code: Bytecode,
    /// Persistent storage slots.
    pub storage: BTreeMap<Word, Word>,
    /// Whether the account was marked for self-destruction.
    pub selfdestructed: bool,
}

impl Default for Account {
    #[inline]
    fn default() -> Self {
        let code = Bytecode::default();
        Self {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: code.hash_slow(),
            code,
            storage: BTreeMap::new(),
            selfdestructed: false,
        }
    }
}

impl Account {
    /// Returns whether this account is empty by the Spurious Dragon definition.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.balance.is_zero() && self.nonce == 0 && self.code.is_empty()
    }

    /// Sets account bytecode and updates the code hash.
    #[inline]
    pub fn set_code(&mut self, code: Bytecode) {
        self.code_hash = code.hash_slow();
        self.code = code;
    }
}

/// Database backing the basic EVM host.
///
/// TODO: Replace this with revm's full database plus journal model. This trait is intentionally
/// small for now and does not represent account warming, storage warming, snapshots, reverts, or
/// original slot values.
pub trait Database {
    /// Loads an account.
    fn account(&mut self, address: Address) -> Option<Account>;

    /// Returns a mutable reference to an account, inserting an empty account if needed.
    fn account_mut(&mut self, address: Address) -> &mut Account;

    /// Loads a historical block hash.
    fn block_hash(&mut self, number: u64) -> Option<B256>;

    /// Loads a transient storage slot.
    fn tload(&mut self, address: Address, key: Word) -> Word;

    /// Stores a transient storage slot.
    fn tstore(&mut self, address: Address, key: Word, value: Word);

    /// Records an emitted log.
    fn log(&mut self, log: Log);
}

/// In-memory database and journal-like host state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryDb {
    /// Persistent accounts keyed by address.
    pub accounts: BTreeMap<Address, Account>,
    /// Historical block hashes keyed by block number.
    pub block_hashes: BTreeMap<u64, B256>,
    /// Transient storage keyed by account and slot.
    pub transient_storage: BTreeMap<(Address, Word), Word>,
    /// Logs emitted during execution.
    pub logs: Vec<Log>,
}

impl MemoryDb {
    /// Returns a reference to an account.
    #[inline]
    pub fn account_ref(&self, address: Address) -> Option<&Account> {
        self.accounts.get(&address)
    }

    /// Returns a mutable reference to an account, inserting an empty account if needed.
    #[inline]
    pub fn account_mut(&mut self, address: Address) -> &mut Account {
        self.accounts.entry(address).or_default()
    }

    /// Inserts or replaces an account.
    #[inline]
    pub fn insert_account(&mut self, address: Address, account: Account) {
        self.accounts.insert(address, account);
    }

    /// Sets a historical block hash.
    #[inline]
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }
}

impl Database for MemoryDb {
    #[inline]
    fn account(&mut self, address: Address) -> Option<Account> {
        self.accounts.get(&address).cloned()
    }

    #[inline]
    fn account_mut(&mut self, address: Address) -> &mut Account {
        Self::account_mut(self, address)
    }

    #[inline]
    fn block_hash(&mut self, number: u64) -> Option<B256> {
        self.block_hashes.get(&number).copied()
    }

    #[inline]
    fn tload(&mut self, address: Address, key: Word) -> Word {
        self.transient_storage.get(&(address, key)).copied().unwrap_or_default()
    }

    #[inline]
    fn tstore(&mut self, address: Address, key: Word, value: Word) {
        self.transient_storage.insert((address, key), value);
    }

    #[inline]
    fn log(&mut self, log: Log) {
        self.logs.push(log);
    }
}
