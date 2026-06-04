//! Basic in-memory EVM host state.

use super::{
    SStore,
    db::{CacheDB, DatabaseCommit, DbResult, DynDatabase},
    eip7708_burn_log,
};
use crate::{
    EvmFeatures, Version,
    bytecode::Bytecode,
    interpreter::{InstrStop, Word},
    storage_key::{StorageKey, StorageKeyMap, StorageKeySet},
};
use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY, Log, U256,
    map::{AddressMap, AddressSet, B256Map, U256Map, hash_map},
};
use core::{convert::Infallible, mem};
use derive_where::derive_where;

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
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T> Tracked<T> {
    /// Creates a tracked value whose original and current values are equal.
    #[inline]
    pub fn new(value: T) -> Self
    where
        T: Clone,
    {
        Self { original: value.clone(), current: value, _non_exhaustive: () }
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
pub struct AccountInfo {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Hash of the raw bytes in `code`, or the empty code hash.
    pub code_hash: B256,
    /// Bytecode associated with this account.
    pub code: Option<Bytecode>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl Default for AccountInfo {
    #[inline]
    fn default() -> Self {
        Self {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: KECCAK256_EMPTY,
            code: Some(Bytecode::default()),
            _non_exhaustive: (),
        }
    }
}

impl AccountInfo {
    /// Creates a new [`AccountInfo`] with the given fields.
    #[inline]
    pub const fn new(balance: Word, nonce: u64, code_hash: B256, code: Bytecode) -> Self {
        Self { balance, nonce, code_hash, code: Some(code), _non_exhaustive: () }
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
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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
            just_created: false,
            code_changed: false,
            _non_exhaustive: (),
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
            _non_exhaustive: (),
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
pub struct StorageOverlay {
    /// Whether consumers must delete all pre-existing storage for the account
    /// before applying individual slot changes.
    pub wiped: bool,
    /// Loaded or changed storage slots.
    pub slots: U256Map<Tracked<Word>>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Complete owned state transition produced by a transaction.
///
/// `StateChanges` is the public, materialized write-set returned in
/// [`crate::TxResult`] and by detached transaction APIs. It is intentionally
/// explicit so embedding clients can update their own database and compute
/// post-state roots without reimplementing EVM account-lifetime rules.
///
/// Logs are execution output rather than database state and are exposed on
/// [`crate::TxOutcome`] and [`crate::TxResult`].
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
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl StateChanges {
    /// Returns whether this transition contains no changes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty() && self.code.is_empty()
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
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Borrowed account information exposed to change sinks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountInfoRef<'a> {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Account code hash.
    pub code_hash: B256,
    /// Borrowed bytecode when the source has it available.
    pub code: Option<&'a Bytecode>,
}

impl<'a> AccountInfoRef<'a> {
    #[inline]
    const fn from_account(account: &'a Account) -> Self {
        Self {
            balance: account.balance,
            nonce: account.nonce,
            code_hash: account.code_hash,
            code: Some(&account.code),
        }
    }

    #[inline]
    const fn from_info(info: &'a AccountInfo) -> Self {
        Self {
            balance: info.balance,
            nonce: info.nonce,
            code_hash: info.code_hash,
            code: info.code.as_ref(),
        }
    }

    /// Materializes this borrowed account into owned account info.
    #[inline]
    pub fn to_account_info(self) -> AccountInfo {
        AccountInfo {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: self.code.cloned(),
            _non_exhaustive: (),
        }
    }

    #[inline]
    const fn to_account_info_without_code(self) -> AccountInfo {
        AccountInfo {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: None,
            _non_exhaustive: (),
        }
    }
}

/// Borrowed account change passed to [`TxChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountChangeRef<'a> {
    /// Account address.
    pub address: Address,
    /// Account at the start of the source's aggregation boundary.
    pub original: Option<AccountInfoRef<'a>>,
    /// Account after the change. `None` is an explicit deletion.
    pub current: Option<AccountInfoRef<'a>>,
}

impl AccountChangeRef<'_> {
    /// Returns whether this change creates an account.
    #[inline]
    pub const fn created(&self) -> bool {
        self.original.is_none() && self.current.is_some()
    }

    /// Returns whether this change deletes an account.
    #[inline]
    pub const fn deleted(&self) -> bool {
        self.original.is_some() && self.current.is_none()
    }
}

/// Borrowed storage change passed to [`TxChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageChangeRef {
    /// Account address.
    pub address: Address,
    /// Storage slot key.
    pub key: Word,
    /// Slot value at the start of the source's aggregation boundary.
    pub original: Word,
    /// Slot value after the change.
    pub current: Word,
    /// Whether this slot is emitted after a storage wipe for the same account.
    pub after_wipe: bool,
}

/// Consumer of borrowed transaction or block state changes.
pub trait TxChangeSink {
    /// Error returned by this sink.
    type Error;

    /// Observes bytecode keyed by code hash.
    #[inline]
    fn bytecode(&mut self, _code_hash: B256, _code: &Bytecode) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes an account change.
    #[inline]
    fn account(&mut self, _change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage wipe marker for an account.
    #[inline]
    fn storage_wipe(&mut self, _address: Address) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage slot change.
    #[inline]
    fn storage(&mut self, _change: StorageChangeRef) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<S> TxChangeSink for &mut S
where
    S: TxChangeSink + ?Sized,
{
    type Error = S::Error;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        (**self).bytecode(code_hash, code)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        (**self).account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        (**self).storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChangeRef) -> Result<(), Self::Error> {
        (**self).storage(change)
    }
}

/// Source of borrowed state changes.
pub trait TxChangeSource {
    /// Visits all changes in deterministic application order.
    fn visit<S: TxChangeSink>(&self, sink: &mut S) -> Result<(), S::Error>;
}

/// Sink that ignores all changes.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopChangeSink;

impl TxChangeSink for NoopChangeSink {
    type Error = Infallible;
}

/// Sink that forwards each change to two sinks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tee<A, B> {
    /// First sink.
    pub a: A,
    /// Second sink.
    pub b: B,
}

impl<A, B> Tee<A, B> {
    /// Creates a new tee sink.
    #[inline]
    pub const fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A, B> TxChangeSink for Tee<A, B>
where
    A: TxChangeSink,
    B: TxChangeSink<Error = A::Error>,
{
    type Error = A::Error;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.a.bytecode(code_hash, code)?;
        self.b.bytecode(code_hash, code)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.a.account(change)?;
        self.b.account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        self.a.storage_wipe(address)?;
        self.b.storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChangeRef) -> Result<(), Self::Error> {
        self.a.storage(change)?;
        self.b.storage(change)
    }
}

/// Block-level account delta accumulated from committed transactions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockAccountDelta {
    /// Account address.
    pub address: Address,
    /// Account at the beginning of the block.
    pub original: Option<AccountInfo>,
    /// Account after the latest committed transaction.
    pub current: Option<AccountInfo>,
    /// Whether storage was wiped for this account during the block.
    pub storage_wiped: bool,
}

/// Block-level storage delta accumulated from committed transactions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockStorageDelta {
    /// Account address.
    pub address: Address,
    /// Storage slot key.
    pub key: Word,
    /// Slot value at the beginning of the block when known. For slots written after a prior
    /// storage wipe without an earlier slot-level change, this is zero and [`Self::after_wipe`]
    /// carries the required wipe-before-write semantics.
    pub original: Word,
    /// Slot value after the latest committed transaction.
    pub current: Word,
    /// Whether this slot was written after a storage wipe.
    pub after_wipe: bool,
}

/// Mutable block-level state accumulator.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlockStateAccumulator {
    accounts: AddressMap<BlockAccountDelta>,
    storage: StorageKeyMap<BlockStorageDelta>,
    code: B256Map<Bytecode>,
}

impl BlockStateAccumulator {
    /// Creates an empty block state accumulator.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the accumulator contains no state changes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.storage.is_empty() && self.code.is_empty()
    }

    /// Freezes the accumulator into immutable block state.
    #[inline]
    pub fn freeze(self) -> FrozenBlockState {
        FrozenBlockState { accounts: self.accounts, storage: self.storage, code: self.code }
    }
}

impl TxChangeSink for BlockStateAccumulator {
    type Error = Infallible;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.code.entry(code_hash).or_insert_with(|| code.clone());
        Ok(())
    }

    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        let original = change.original.map(AccountInfoRef::to_account_info_without_code);
        let current = change.current.map(AccountInfoRef::to_account_info_without_code);
        match self.accounts.entry(change.address) {
            hash_map::Entry::Occupied(mut entry) => {
                let delta = entry.get_mut();
                if delta.original.is_none() && delta.current.is_none() && delta.storage_wiped {
                    delta.original = original;
                }
                delta.current = current;
                if delta.original.is_none() {
                    delta.storage_wiped = false;
                }
                if delta.original == delta.current && !delta.storage_wiped {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if original != current {
                    entry.insert(BlockAccountDelta {
                        address: change.address,
                        original,
                        current,
                        storage_wiped: false,
                    });
                }
            }
        }
        Ok(())
    }

    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        let record_wipe = self.accounts.get(&address).is_none_or(|delta| delta.original.is_some());
        if record_wipe {
            self.accounts
                .entry(address)
                .and_modify(|delta| delta.storage_wiped = true)
                .or_insert_with(|| BlockAccountDelta {
                    address,
                    original: None,
                    current: None,
                    storage_wiped: true,
                });
        }

        self.storage.retain(|_, delta| {
            if delta.address != address {
                return true;
            }
            delta.current = Word::ZERO;
            delta.after_wipe = true;
            delta.original != Word::ZERO && record_wipe
        });
        Ok(())
    }

    fn storage(&mut self, change: StorageChangeRef) -> Result<(), Self::Error> {
        let storage_key = StorageKey::new(change.address, change.key);
        let after_wipe = change.after_wipe
            || self.accounts.get(&change.address).is_some_and(|delta| delta.storage_wiped);
        match self.storage.entry(storage_key) {
            hash_map::Entry::Occupied(mut entry) => {
                let delta = entry.get_mut();
                delta.current = change.current;
                delta.after_wipe |= after_wipe;
                let subsumed_by_wipe = delta.after_wipe && delta.current.is_zero();
                if (!delta.after_wipe && delta.original == delta.current) || subsumed_by_wipe {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if (!after_wipe && change.original == change.current)
                    || (after_wipe && change.current.is_zero())
                {
                    return Ok(());
                }
                entry.insert(BlockStorageDelta {
                    address: change.address,
                    key: change.key,
                    original: change.original,
                    current: change.current,
                    after_wipe,
                });
            }
        }
        Ok(())
    }
}

impl TxChangeSource for BlockStateAccumulator {
    #[inline]
    fn visit<S: TxChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        visit_block_changes(&self.accounts, &self.storage, &self.code, sink)
    }
}

fn visit_block_changes<S: TxChangeSink>(
    accounts: &AddressMap<BlockAccountDelta>,
    storage: &StorageKeyMap<BlockStorageDelta>,
    code: &B256Map<Bytecode>,
    sink: &mut S,
) -> Result<(), S::Error> {
    let mut code_entries = code.iter().collect::<Vec<_>>();
    code_entries.sort_by_key(|(code_hash, _)| **code_hash);
    for (&code_hash, code) in code_entries {
        sink.bytecode(code_hash, code)?;
    }

    let mut account_deltas = accounts.values().collect::<Vec<_>>();
    account_deltas.sort_by_key(|delta| delta.address);
    for delta in &account_deltas {
        if delta.storage_wiped {
            sink.storage_wipe(delta.address)?;
        }
    }

    let mut storage_deltas = storage.values().collect::<Vec<_>>();
    storage_deltas.sort_by_key(|delta| (delta.address, delta.key));
    for delta in storage_deltas {
        sink.storage(StorageChangeRef {
            address: delta.address,
            key: delta.key,
            original: delta.original,
            current: delta.current,
            after_wipe: delta.after_wipe,
        })?;
    }

    for delta in account_deltas {
        if delta.original != delta.current {
            sink.account(AccountChangeRef {
                address: delta.address,
                original: delta.original.as_ref().map(AccountInfoRef::from_info),
                current: delta.current.as_ref().map(AccountInfoRef::from_info),
            })?;
        }
    }
    Ok(())
}

/// Immutable block state produced by [`BlockStateAccumulator::freeze`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrozenBlockState {
    accounts: AddressMap<BlockAccountDelta>,
    storage: StorageKeyMap<BlockStorageDelta>,
    code: B256Map<Bytecode>,
}

impl FrozenBlockState {
    /// Returns account deltas in arbitrary map order.
    #[inline]
    pub fn accounts(&self) -> impl Iterator<Item = &BlockAccountDelta> {
        self.accounts.values()
    }

    /// Returns storage deltas in arbitrary map order.
    #[inline]
    pub fn storage(&self) -> impl Iterator<Item = &BlockStorageDelta> {
        self.storage.values()
    }

    /// Returns bytecode entries in arbitrary map order.
    #[inline]
    pub fn code(&self) -> impl Iterator<Item = (&B256, &Bytecode)> {
        self.code.iter()
    }

    /// Returns account deltas sorted by address.
    pub fn accounts_sorted(&self) -> Vec<&BlockAccountDelta> {
        let mut accounts = self.accounts.values().collect::<Vec<_>>();
        accounts.sort_by_key(|delta| delta.address);
        accounts
    }

    /// Returns storage deltas sorted by address and slot.
    pub fn storage_sorted(&self) -> Vec<&BlockStorageDelta> {
        let mut storage = self.storage.values().collect::<Vec<_>>();
        storage.sort_by_key(|delta| (delta.address, delta.key));
        storage
    }
}

impl TxChangeSource for FrozenBlockState {
    #[inline]
    fn visit<S: TxChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        visit_block_changes(&self.accounts, &self.storage, &self.code, sink)
    }
}

impl TxChangeSource for StateChanges {
    fn visit<S: TxChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        for (&code_hash, code) in &self.code {
            sink.bytecode(code_hash, code)?;
        }
        for (&address, storage) in &self.storage {
            if storage.wipe {
                sink.storage_wipe(address)?;
            }
            for (&key, slot) in &storage.slots {
                sink.storage(StorageChangeRef {
                    address,
                    key,
                    original: slot.original,
                    current: slot.current,
                    after_wipe: storage.wipe,
                })?;
            }
        }
        for (&address, change) in &self.accounts {
            sink.account(AccountChangeRef {
                address,
                original: change.original.as_ref().map(AccountInfoRef::from_info),
                current: change.current.as_ref().map(AccountInfoRef::from_info),
            })?;
        }
        Ok(())
    }
}

/// State checkpoint for reverting state changes.
#[allow(missing_copy_implementations)]
#[derive(Debug, Eq, PartialEq)]
pub struct StateCheckpoint {
    /// Revert journal length at the checkpoint.
    journal_len: usize,
    /// Emitted log count at the checkpoint.
    logs_len: usize,
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

/// Reusable transaction-local state.
#[derive(Debug, Default)]
struct Scratch {
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

impl Scratch {
    /// Clears transaction-scoped substate while retaining allocated buffers.
    fn clear_transaction_state(&mut self) {
        self.accounts.clear();
        self.storage.clear();
        self.journal.clear();
        self.touched.clear();
        self.selfdestructs.clear();
        self.accessed_accounts.clear();
        self.accessed_storage.clear();
        self.transient_storage.clear();
        self.logs.clear();
    }
}

/// Mutable EVM state with an accepted-state cache, transaction scratch, and reversible journal.
#[derive_where(Debug)]
#[non_exhaustive]
pub struct State {
    /// Database plus accepted transaction-boundary state overlay.
    #[derive_where(skip)]
    database: CacheDB<Box<dyn DynDatabase>>,
    /// Reusable transaction-local state.
    scratch: Scratch,
}

impl State {
    /// Creates a new state over an initial database.
    pub fn new(initial: impl DynDatabase) -> Self {
        Self::new_mono(Box::new(initial))
    }

    pub(crate) fn new_mono(initial: Box<dyn DynDatabase>) -> Self {
        Self { database: CacheDB::new(initial), scratch: Scratch::default() }
    }

    /// Returns a checkpoint for later rollback.
    #[inline]
    pub const fn checkpoint(&self) -> StateCheckpoint {
        StateCheckpoint {
            journal_len: self.scratch.journal.len(),
            logs_len: self.scratch.logs.len(),
        }
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

    /// Returns the accepted database overlay.
    #[inline]
    pub const fn overlay_db(&self) -> &CacheDB<Box<dyn DynDatabase>> {
        &self.database
    }

    /// Returns the accepted database overlay mutably.
    #[inline]
    pub const fn overlay_db_mut(&mut self) -> &mut CacheDB<Box<dyn DynDatabase>> {
        &mut self.database
    }

    /// Replaces the initial database and clears all in-memory state layers.
    #[inline]
    pub fn set_initial(&mut self, initial: impl DynDatabase) {
        self.database = CacheDB::new(Box::new(initial));
        self.clear_transaction_state();
    }

    /// Applies borrowed changes to the accepted state overlay.
    #[inline]
    pub fn commit_source<S: TxChangeSource>(&mut self, source: &S) {
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

    /// Records a transaction log.
    #[inline]
    pub fn log(&mut self, log: Log) {
        self.scratch.logs.push(log);
    }

    /// Returns a loaded persistent storage overlay slot, if present.
    ///
    /// This is a non-mutating overlay lookup. It does not load the account or slot from the
    /// backing database; use [`Self::storage`] when database-backed loading is desired.
    #[inline]
    pub fn storage_ref(&self, address: &Address, key: &Word) -> Option<Word> {
        if let Some(storage) = self.scratch.storage.get(address) {
            if let Some(slot) = storage.slots.get(key) {
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
        self.scratch.accounts.get(address)?.as_ref()
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

    fn load_account(&mut self, address: &Address) -> DbResult<Option<Account>> {
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

    /// Returns account info.
    #[inline(never)]
    pub fn account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        if let Some(account) = self.scratch.accounts.get(address) {
            return Ok(account.as_ref().map(Account::info));
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
        Ok(self.account_info(address)?.is_none() && !self.scratch.touched.contains(address))
    }

    /// Returns an account if it exists.
    pub fn find(&mut self, address: &Address) -> DbResult<Option<&Account>> {
        let account = Self::ensure_transaction_account(
            &mut self.database,
            &mut self.scratch.accounts,
            &mut self.scratch.journal,
            address,
        )?;
        Ok(account.as_ref())
    }

    /// Gets account code.
    pub fn get_code(&mut self, address: &Address) -> DbResult<Bytecode> {
        if let Some(account) = self.scratch.accounts.get(address).and_then(Option::as_ref) {
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
        if let Some(storage) = self.scratch.storage.get(address) {
            if let Some(slot) = storage.slots.get(key) {
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
                    entry.get_mut().current = value;
                    self.scratch.journal.push(JournalEntry::StorageChange {
                        address: *address,
                        key: *key,
                        previous,
                    });
                }
            }
            hash_map::Entry::Vacant(entry) => {
                entry.insert(Tracked { original, current: value, _non_exhaustive: () });
                self.scratch
                    .journal
                    .push(JournalEntry::StorageInserted { address: *address, key: *key });
            }
        }
    }

    /// Loads persistent storage.
    pub fn storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let Some(_) = self.account_info(address)? else {
            return Ok(Word::ZERO);
        };
        self.current_storage(address, key)
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
    pub(super) fn is_created_in_transaction(&self, address: &Address) -> bool {
        self.account_ref(address).is_some_and(|account| account.just_created)
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
                    self.scratch.accounts.remove(&address);
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
                        slot.current = previous;
                    }
                }
                JournalEntry::StorageInserted { address, key } => {
                    if let Some(storage) = self.scratch.storage.get_mut(&address) {
                        storage.slots.remove(&key);
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
        Ok(self.load_account(address)?.as_ref().is_some_and(Account::is_empty))
    }

    fn account_exists(&mut self, address: &Address) -> DbResult<bool> {
        if let Some(account) = self.scratch.accounts.get(address) {
            return Ok(account.is_some());
        }
        Ok(self.load_account(address)?.is_some())
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
        let original_exists = self.load_account(address)?.is_some();
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

        self.scratch.selfdestructs = selfdestructs;
        self.scratch.selfdestructs.clear();

        self.scratch.touched = touched;
        self.scratch.touched.clear();
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

    /// Visits pending transaction changes in database application order.
    ///
    /// This borrows changes directly from the transaction layer. It does not materialize
    /// [`StateChanges`] and does not mutate the accepted overlay.
    pub(crate) fn visit_transaction_changes<S: TxChangeSink>(
        &self,
        sink: &mut S,
    ) -> Result<(), S::Error> {
        for current in self.scratch.accounts.values().flatten() {
            let code_hash = current.code_hash;
            if current.code_changed
                && !current.code.is_empty()
                && !code_hash.is_zero()
                && code_hash != KECCAK256_EMPTY
            {
                sink.bytecode(code_hash, &current.code)?;
            }
        }

        for (&address, storage) in &self.scratch.storage {
            if storage.wiped {
                sink.storage_wipe(address)?;
            }
            for (&key, slot) in &storage.slots {
                if slot.original != slot.current && (!storage.wiped || !slot.current.is_zero()) {
                    sink.storage(StorageChangeRef {
                        address,
                        key,
                        original: slot.original,
                        current: slot.current,
                        after_wipe: storage.wiped,
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
    /// exposed through [`crate::TxOutcome`] and [`crate::TxResult`].
    pub(crate) fn build_state_changes(&mut self) -> StateChanges {
        let mut changes = StateChanges::default();

        for (&address, current) in &self.scratch.accounts {
            let original = self.database.account_info(&address);
            let current = current.as_ref();
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

        for (&address, storage) in &self.scratch.storage {
            let mut set = StorageChangeSet {
                wipe: storage.wiped,
                slots: BTreeMap::new(),
                _non_exhaustive: (),
            };
            for (&key, slot) in &storage.slots {
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

    /// Accepts the current transaction's state transition without materializing it.
    pub(crate) fn commit_transaction(&mut self) {
        for current in self.scratch.accounts.values().flatten() {
            let code_hash = current.code_hash;
            if current.code_changed
                && !current.code.is_empty()
                && !code_hash.is_zero()
                && code_hash != KECCAK256_EMPTY
            {
                self.database.cache.contracts.insert(code_hash, current.code.clone());
            }
        }

        for (&address, storage) in &self.scratch.storage {
            if storage.wiped {
                self.database.cache.storage.entry(address).or_default().wipe();
            }
            for (&key, slot) in &storage.slots {
                if slot.original == slot.current || (storage.wiped && slot.current.is_zero()) {
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
    pub(crate) fn accept_transaction(&mut self) -> StateChanges {
        let changes = self.build_state_changes();
        self.database.commit(&changes);
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
    pub(super) fn commit_transaction_overlay(&mut self) {
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

        assert_eq!(state.storage(&address, &Word::from(1)).unwrap(), Word::from(30));
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.storage(&address, &Word::from(1)).unwrap(), Word::from(10));
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
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
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

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
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

        assert!(!changes.accounts.contains_key(&address));
        assert!(!changes.storage.contains_key(&address));
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

        assert!(state.scratch.touched.is_empty());
        assert!(state.scratch.selfdestructs.is_empty());
        assert_eq!(state.scratch.touched.capacity(), touched_capacity);
        assert_eq!(state.scratch.selfdestructs.capacity(), selfdestructs_capacity);
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
        assert!(changes.storage.get(&address).is_some_and(|storage| storage.wipe));
    }

    fn account_change(
        address: Address,
        original: Option<AccountInfo>,
        current: Option<AccountInfo>,
    ) -> StateChanges {
        let mut changes = StateChanges::default();
        changes.accounts.insert(address, Tracked { original, current, _non_exhaustive: () });
        changes
    }

    fn storage_change(
        address: Address,
        key: Word,
        original: Word,
        current: Word,
        wipe: bool,
    ) -> StateChanges {
        let mut changes = StateChanges::default();
        changes.storage.insert(
            address,
            StorageChangeSet {
                wipe,
                slots: BTreeMap::from([(key, Tracked { original, current, _non_exhaustive: () })]),
                _non_exhaustive: (),
            },
        );
        changes
    }

    fn storage_wipe(address: Address) -> StateChanges {
        let mut changes = StateChanges::default();
        changes.storage.insert(
            address,
            StorageChangeSet { wipe: true, slots: BTreeMap::new(), _non_exhaustive: () },
        );
        changes
    }

    fn without_code(mut info: AccountInfo) -> AccountInfo {
        info.code = None;
        info
    }

    #[test]
    fn block_accumulator_collapses_create_then_delete() {
        let address = Address::from([0x50; 20]);
        let key = Word::from(1);
        let created = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        let mut create = account_change(address, None, Some(created.clone()));
        create.storage = storage_change(address, key, Word::ZERO, Word::from(7), true).storage;
        create.visit(&mut accumulator).expect("block accumulator is infallible");

        let mut delete = account_change(address, Some(created), None);
        delete.storage = storage_wipe(address).storage;
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        let frozen = accumulator.freeze();
        assert!(frozen.accounts_sorted().is_empty());
        assert!(frozen.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_preserves_original_for_delete_then_recreate() {
        let address = Address::from([0x51; 20]);
        let key = Word::from(1);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let recreated = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        let mut delete = account_change(address, Some(original.clone()), None);
        delete.storage = storage_wipe(address).storage;
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        let mut create = account_change(address, None, Some(recreated.clone()));
        create.storage = storage_change(address, key, Word::ZERO, Word::from(7), true).storage;
        create.visit(&mut accumulator).expect("block accumulator is infallible");

        let frozen = accumulator.freeze();
        let accounts = frozen.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].original.as_ref(), Some(&without_code(original)));
        assert_eq!(accounts[0].current.as_ref(), Some(&without_code(recreated)));
        assert!(accounts[0].storage_wiped);

        let storage = frozen.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].key, key);
        assert_eq!(storage[0].current, Word::from(7));
        assert!(storage[0].after_wipe);
    }

    #[test]
    fn block_accumulator_collapses_storage_wipe_write_wipe() {
        let address = Address::from([0x52; 20]);
        let key = Word::from(1);
        let mut accumulator = BlockStateAccumulator::new();

        let first = storage_change(address, key, Word::from(5), Word::from(7), true);
        first.visit(&mut accumulator).expect("block accumulator is infallible");
        storage_wipe(address).visit(&mut accumulator).expect("block accumulator is infallible");

        let frozen = accumulator.freeze();
        let accounts = frozen.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert!(accounts[0].storage_wiped);
        assert!(frozen.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_keeps_account_only_and_storage_only_changes_separate() {
        let account_address = Address::from([0x53; 20]);
        let storage_address = Address::from([0x54; 20]);
        let key = Word::from(1);
        let original = AccountInfo::default().with_balance(Word::from(1));
        let current = AccountInfo::default().with_balance(Word::from(2));
        let mut accumulator = BlockStateAccumulator::new();

        account_change(account_address, Some(original.clone()), Some(current.clone()))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        storage_change(storage_address, key, Word::from(3), Word::from(4), false)
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        let frozen = accumulator.freeze();
        let accounts = frozen.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].address, account_address);
        assert_eq!(accounts[0].original.as_ref(), Some(&without_code(original)));
        assert_eq!(accounts[0].current.as_ref(), Some(&without_code(current)));
        assert!(!accounts[0].storage_wiped);

        let storage = frozen.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].address, storage_address);
        assert_eq!(storage[0].key, key);
        assert_eq!(storage[0].original, Word::from(3));
        assert_eq!(storage[0].current, Word::from(4));
        assert!(!storage[0].after_wipe);
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
        let _changes = state.build_state_changes();
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
