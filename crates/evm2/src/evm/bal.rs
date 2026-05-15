//! Block access list state and database support.

use super::{
    AccountInfo, DatabaseCommit, DbErrorCode, DbResult, DynDatabase, StateChanges,
    StorageChangeSet, db::bal_error_code,
};
use crate::{
    bytecode::{Bytecode, BytecodeDecodeError},
    interpreter::Word,
};
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use alloy_eip7928::{
    AccountChanges as AlloyAccountChanges, BalanceChange as AlloyBalanceChange, BlockAccessIndex,
    BlockAccessList as AlloyBal, CodeChange as AlloyCodeChange, NonceChange as AlloyNonceChange,
    SlotChanges as AlloySlotChanges, StorageChange as AlloyStorageChange,
};
use alloy_primitives::{Address, B256};
use core::{error::Error, fmt};

/// Positional account id inside a block access list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountId(usize);

impl AccountId {
    /// Creates a new account id.
    #[inline]
    pub const fn new(id: usize) -> Self {
        Self(id)
    }

    /// Returns the raw account id.
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Error returned when a BAL lookup cannot find expected data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalError {
    /// The address was not present in the BAL's accounts map.
    AccountNotFound {
        /// Address that was not found.
        address: Address,
    },
    /// The supplied account id index is out of range for the BAL's accounts map.
    InvalidAccountId {
        /// Account id that was supplied.
        account_id: AccountId,
    },
    /// The account exists in the BAL but the requested storage slot is not listed under it.
    SlotNotFound {
        /// Address of the account whose slot was missing.
        address: Address,
        /// Storage slot that was not found.
        slot: Word,
    },
}

impl fmt::Display for BalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccountNotFound { address } => write!(f, "Account {address} not found in BAL"),
            Self::InvalidAccountId { account_id } => {
                write!(f, "Invalid BAL account id {}", account_id.get())
            }
            Self::SlotNotFound { address, slot } => {
                write!(f, "Slot {slot:#x} not found in BAL for account {address}")
            }
        }
    }
}

impl Error for BalError {}

/// List of block-indexed writes for one state item.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BalWrites<T: PartialEq + Clone> {
    /// Writes keyed by block access index.
    pub writes: Vec<(BlockAccessIndex, T)>,
}

impl<T: PartialEq + Clone> BalWrites<T> {
    /// Creates a write list sorted by block access index.
    pub fn new(mut writes: Vec<(BlockAccessIndex, T)>) -> Self {
        writes.sort_unstable_by_key(|(index, _)| *index);
        Self { writes }
    }

    #[inline(never)]
    fn get_linear_search(&self, bal_index: BlockAccessIndex) -> Option<T> {
        let mut last_item = None;
        for (index, item) in &self.writes {
            if index >= &bal_index {
                return last_item;
            }
            last_item = Some(item.clone());
        }
        last_item
    }

    /// Returns the latest write before `bal_index`.
    pub fn get(&self, bal_index: BlockAccessIndex) -> Option<T> {
        if self.writes.len() < 5 {
            return self.get_linear_search(bal_index);
        }
        let i = match self.writes.binary_search_by_key(&bal_index, |(index, _)| *index) {
            Ok(i) | Err(i) => i,
        };
        (i != 0).then(|| self.writes[i - 1].1.clone())
    }

    /// Extends this write list with another one.
    pub fn extend(&mut self, other: Self) {
        self.writes.extend(other.writes);
        self.writes.sort_unstable_by_key(|(index, _)| *index);
    }

    /// Returns whether this item has no writes.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// Inserts a write without comparing to an original value.
    #[inline]
    pub fn force_update(&mut self, index: BlockAccessIndex, value: T) {
        if let Some(last) = self.writes.last_mut()
            && last.0 == index
        {
            last.1 = value;
            return;
        }
        self.writes.push((index, value));
    }

    /// Inserts a write if `value` differs from the previous value.
    pub fn update(&mut self, index: BlockAccessIndex, original_value: &T, value: T) {
        self.update_with_key(index, original_value, value, |value| value);
    }

    /// Inserts a write, comparing by a projected key.
    pub fn update_with_key<K: PartialEq, F>(
        &mut self,
        index: BlockAccessIndex,
        original_subvalue: &K,
        value: T,
        f: F,
    ) where
        F: Fn(&T) -> &K,
    {
        if let Some(last) = self.writes.last_mut()
            && last.0 != index
        {
            if f(&last.1) != f(&value) {
                self.writes.push((index, value));
            }
            return;
        }

        let (previous, last) = match self.writes.as_mut_slice() {
            [.., previous, last] => (f(&previous.1), last),
            [last] => (original_subvalue, last),
            [] => {
                if original_subvalue != f(&value) {
                    self.writes.push((index, value));
                }
                return;
            }
        };

        if previous == f(&value) {
            self.writes.pop();
        } else {
            last.1 = value;
        }
    }
}

impl From<Vec<AlloyBalanceChange>> for BalWrites<Word> {
    fn from(changes: Vec<AlloyBalanceChange>) -> Self {
        Self::new(
            changes
                .into_iter()
                .map(|change| (change.block_access_index, change.post_balance))
                .collect(),
        )
    }
}

impl From<Vec<AlloyNonceChange>> for BalWrites<u64> {
    fn from(changes: Vec<AlloyNonceChange>) -> Self {
        Self::new(
            changes
                .into_iter()
                .map(|change| (change.block_access_index, change.new_nonce))
                .collect(),
        )
    }
}

impl TryFrom<Vec<AlloyCodeChange>> for BalWrites<(B256, Bytecode)> {
    type Error = BytecodeDecodeError;

    fn try_from(changes: Vec<AlloyCodeChange>) -> Result<Self, Self::Error> {
        let mut writes = Vec::with_capacity(changes.len());
        for change in changes {
            let code = Bytecode::new_raw_checked(change.new_code)?;
            writes.push((change.block_access_index, (code.hash_slow(), code)));
        }
        Ok(Self::new(writes))
    }
}

impl From<Vec<AlloyStorageChange>> for BalWrites<Word> {
    fn from(changes: Vec<AlloyStorageChange>) -> Self {
        Self::new(
            changes
                .into_iter()
                .map(|change| (change.block_access_index, change.new_value))
                .collect(),
        )
    }
}

/// Account-info BAL data.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountInfoBal {
    /// Nonce writes.
    pub nonce: BalWrites<u64>,
    /// Balance writes.
    pub balance: BalWrites<Word>,
    /// Code writes.
    pub code: BalWrites<(B256, Bytecode)>,
}

impl AccountInfoBal {
    /// Populates account info from BAL writes before `bal_index`.
    pub fn populate_account_info(
        &self,
        bal_index: BlockAccessIndex,
        account: &mut AccountInfo,
    ) -> bool {
        let mut changed = false;
        if let Some(nonce) = self.nonce.get(bal_index) {
            account.nonce = nonce;
            changed = true;
        }
        if let Some(balance) = self.balance.get(bal_index) {
            account.balance = balance;
            changed = true;
        }
        if let Some((code_hash, code)) = self.code.get(bal_index) {
            account.code_hash = code_hash;
            account.code = Some(code);
            changed = true;
        }
        changed
    }

    #[inline]
    fn update(&mut self, index: BlockAccessIndex, original: &AccountInfo, current: &AccountInfo) {
        self.nonce.update(index, &original.nonce, current.nonce);
        self.balance.update(index, &original.balance, current.balance);
        if original.code_hash != current.code_hash {
            self.code.update_with_key(
                index,
                &original.code_hash,
                (current.code_hash, current.code.clone().unwrap_or_default()),
                |value| &value.0,
            );
        }
    }
}

/// Storage BAL data for an account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageBal {
    /// Storage slots with reads or writes.
    pub storage: BTreeMap<Word, BalWrites<Word>>,
}

impl StorageBal {
    /// Gets storage from BAL.
    #[inline]
    pub fn get(
        &self,
        address: &Address,
        key: Word,
        bal_index: BlockAccessIndex,
    ) -> Result<Option<Word>, BalError> {
        Ok(self.get_bal_writes(address, key)?.get(bal_index))
    }

    /// Gets storage writes for a slot.
    #[inline]
    pub fn get_bal_writes(
        &self,
        address: &Address,
        key: Word,
    ) -> Result<&BalWrites<Word>, BalError> {
        self.storage.get(&key).ok_or(BalError::SlotNotFound { address: *address, slot: key })
    }

    #[inline]
    fn update(&mut self, index: BlockAccessIndex, storage: &StorageChangeSet) {
        for (&key, value) in &storage.slots {
            self.storage.entry(key).or_default().update(index, &value.original, value.current);
        }
    }

    #[inline]
    fn update_reads(&mut self, storage: impl Iterator<Item = Word>) {
        for key in storage {
            self.storage.entry(key).or_default();
        }
    }
}

impl FromIterator<(Word, BalWrites<Word>)> for StorageBal {
    fn from_iter<I: IntoIterator<Item = (Word, BalWrites<Word>)>>(iter: I) -> Self {
        Self { storage: iter.into_iter().collect() }
    }
}

/// BAL data for one account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountBal {
    /// Account info changes.
    pub account_info: AccountInfoBal,
    /// Storage changes and reads.
    pub storage: StorageBal,
}

impl AccountBal {
    /// Populates account info from BAL.
    #[inline]
    pub fn populate_account_info(
        &self,
        bal_index: BlockAccessIndex,
        account: &mut AccountInfo,
    ) -> bool {
        self.account_info.populate_account_info(bal_index, account)
    }

    /// Creates account BAL data from an Alloy account.
    pub fn try_from_alloy(
        account: AlloyAccountChanges,
    ) -> Result<(Address, Self), BytecodeDecodeError> {
        Ok((
            account.address,
            Self {
                account_info: AccountInfoBal {
                    nonce: account.nonce_changes.into(),
                    balance: account.balance_changes.into(),
                    code: account.code_changes.try_into()?,
                },
                storage: account
                    .storage_changes
                    .into_iter()
                    .map(|slot| (slot.slot, slot.changes.into()))
                    .chain(account.storage_reads.into_iter().map(|key| (key, BalWrites::default())))
                    .collect(),
            },
        ))
    }

    /// Converts account BAL data into Alloy BAL data.
    pub fn into_alloy_account(self, address: Address) -> AlloyAccountChanges {
        let mut storage_changes = Vec::new();
        let mut storage_reads = Vec::new();
        for (slot, value) in self.storage.storage {
            if value.writes.is_empty() {
                storage_reads.push(slot);
            } else {
                let mut changes = value
                    .writes
                    .into_iter()
                    .map(|(index, value)| AlloyStorageChange::new(index, value))
                    .collect::<Vec<_>>();
                changes.sort_unstable_by_key(|change| change.block_access_index);
                storage_changes.push(AlloySlotChanges::new(slot, changes));
            }
        }
        let mut account = AlloyAccountChanges {
            address,
            storage_changes,
            storage_reads,
            balance_changes: self
                .account_info
                .balance
                .writes
                .into_iter()
                .map(|(index, value)| AlloyBalanceChange::new(index, value))
                .collect(),
            nonce_changes: self
                .account_info
                .nonce
                .writes
                .into_iter()
                .map(|(index, value)| AlloyNonceChange::new(index, value))
                .collect(),
            code_changes: self
                .account_info
                .code
                .writes
                .into_iter()
                .map(|(index, (_, value))| AlloyCodeChange::new(index, value.original_bytes()))
                .collect(),
        };
        account.sort();
        account
    }
}

/// Block access list state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bal {
    /// Accounts keyed by address.
    pub accounts: BTreeMap<Address, AccountBal>,
}

impl Bal {
    /// Creates an empty BAL.
    #[inline]
    pub const fn new() -> Self {
        Self { accounts: BTreeMap::new() }
    }

    /// Converts Alloy BAL data into BAL state.
    pub fn try_from_alloy(alloy_bal: AlloyBal) -> Result<Self, BytecodeDecodeError> {
        alloy_bal.into_iter().map(AccountBal::try_from_alloy).collect()
    }

    /// Clones Alloy BAL data into BAL state.
    pub fn clone_from_alloy(alloy_bal: &AlloyBal) -> Result<Self, BytecodeDecodeError> {
        alloy_bal.clone().into_iter().map(AccountBal::try_from_alloy).collect()
    }

    /// Updates this BAL from a transaction or system-call state transition.
    pub fn push_state_changes(&mut self, index: BlockAccessIndex, changes: &StateChanges) {
        for (&address, account) in &changes.accounts {
            let bal_account = self.accounts.entry(address).or_default();
            let empty = AccountInfo::default();
            let original = account.original.as_ref().unwrap_or(&empty);
            let current = account.current.as_ref().unwrap_or(&empty);
            bal_account.account_info.update(index, original, current);
        }

        for (&address, storage) in &changes.storage {
            self.accounts.entry(address).or_default().storage.update(index, storage);
        }

        if let Some(accesses) = &changes.accesses {
            for &address in &accesses.accounts {
                self.accounts.entry(address).or_default();
            }
            for (&address, slots) in &accesses.storage {
                let written = changes.storage.get(&address);
                let slots = slots.iter().copied().filter(|slot| {
                    !written.is_some_and(|storage| storage.slots.contains_key(slot))
                });
                self.accounts.entry(address).or_default().storage.update_reads(slots);
            }
        }
    }

    /// Populates account info from BAL.
    pub fn populate_account_info(
        &self,
        address: &Address,
        bal_index: BlockAccessIndex,
        account: &mut AccountInfo,
    ) -> Result<bool, BalError> {
        let Some(bal_account) = self.accounts.get(address) else {
            return Err(BalError::AccountNotFound { address: *address });
        };
        Ok(bal_account.populate_account_info(bal_index, account))
    }

    /// Populates storage from BAL.
    pub fn populate_storage_slot(
        &self,
        address: Address,
        bal_index: BlockAccessIndex,
        key: Word,
        value: &mut Word,
    ) -> Result<(), BalError> {
        let Some(bal_account) = self.accounts.get(&address) else {
            return Err(BalError::AccountNotFound { address });
        };
        if let Some(bal_value) = bal_account.storage.get(&address, key, bal_index)? {
            *value = bal_value;
        }
        Ok(())
    }

    /// Gets a storage value from BAL if a prior write exists.
    pub fn storage(
        &self,
        address: &Address,
        key: Word,
        bal_index: BlockAccessIndex,
    ) -> Result<Option<Word>, BalError> {
        let Some(bal_account) = self.accounts.get(address) else {
            return Err(BalError::AccountNotFound { address: *address });
        };
        bal_account.storage.get(address, key, bal_index)
    }

    /// Consumes this BAL and returns canonical Alloy BAL data.
    pub fn into_alloy_bal(self) -> AlloyBal {
        let mut bal = self
            .accounts
            .into_iter()
            .map(|(address, account)| account.into_alloy_account(address))
            .collect::<Vec<_>>();
        bal.sort_unstable_by_key(|account| account.address);
        bal
    }

    /// Consumes this BAL builder and returns canonical Alloy BAL data.
    #[inline]
    pub fn build(self) -> AlloyBal {
        self.into_alloy_bal()
    }
}

impl FromIterator<(Address, AccountBal)> for Bal {
    fn from_iter<I: IntoIterator<Item = (Address, AccountBal)>>(iter: I) -> Self {
        Self { accounts: iter.into_iter().collect() }
    }
}

/// Backwards-compatible name for building BALs.
pub type BalBuilder = Bal;

/// BAL read/build state for a database.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BalState {
    /// BAL used to execute transactions.
    pub bal: Option<Arc<Bal>>,
    /// BAL builder used to build a BAL from committed changes.
    pub bal_builder: Option<Bal>,
    /// Current block access index.
    pub bal_index: BlockAccessIndex,
}

impl BalState {
    /// Creates empty BAL state.
    #[inline]
    pub const fn new() -> Self {
        Self { bal: None, bal_builder: None, bal_index: BlockAccessIndex::PRE_EXECUTION }
    }

    /// Sets the read BAL.
    #[inline]
    pub fn set_bal(&mut self, bal: Option<Arc<Bal>>) {
        self.bal = bal;
    }

    /// Enables BAL building.
    #[inline]
    pub fn enable_bal_builder(&mut self) {
        self.bal_builder = Some(Bal::new());
    }

    /// Resets the current BAL index.
    #[inline]
    pub const fn reset_bal_index(&mut self) {
        self.bal_index = BlockAccessIndex::PRE_EXECUTION;
    }

    /// Sets the current BAL index.
    #[inline]
    pub const fn set_bal_index(&mut self, index: BlockAccessIndex) {
        self.bal_index = index;
    }

    /// Bumps the current BAL index.
    #[inline]
    pub const fn bump_bal_index(&mut self) {
        self.bal_index.increment();
    }

    /// Takes the built BAL.
    #[inline]
    pub const fn take_built_bal(&mut self) -> Option<Bal> {
        self.reset_bal_index();
        self.bal_builder.take()
    }

    /// Takes the built BAL as canonical Alloy BAL data.
    #[inline]
    pub fn take_built_alloy_bal(&mut self) -> Option<AlloyBal> {
        self.take_built_bal().map(Bal::into_alloy_bal)
    }

    fn account(
        &self,
        address: &Address,
        account: &mut Option<AccountInfo>,
    ) -> Result<bool, BalError> {
        let Some(bal) = &self.bal else {
            return Ok(false);
        };
        let is_none = account.is_none();
        let mut bal_account = account.take().unwrap_or_default();
        let changed = bal.populate_account_info(address, self.bal_index, &mut bal_account)?;
        if !changed && is_none {
            return Ok(true);
        }
        *account = Some(bal_account);
        Ok(true)
    }

    fn storage(&self, address: &Address, key: &Word) -> Result<Option<Word>, BalError> {
        let Some(bal) = &self.bal else {
            return Ok(None);
        };
        bal.storage(address, *key, self.bal_index)
    }

    fn record_state_changes(&mut self, changes: &StateChanges) {
        if let Some(bal_builder) = &mut self.bal_builder {
            bal_builder.push_state_changes(self.bal_index, changes);
        }
    }
}

/// Database wrapper with BAL read/build state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BalDatabase<DB> {
    /// BAL manager.
    pub bal_state: BalState,
    /// Wrapped backing database.
    pub db: DB,
    bal_error: Option<BalError>,
}

impl<DB> BalDatabase<DB> {
    /// Creates a BAL database wrapper.
    #[inline]
    pub fn new(db: DB) -> Self {
        Self { bal_state: BalState::default(), db, bal_error: None }
    }

    /// Sets the read BAL.
    #[inline]
    pub fn with_bal(mut self, bal: Arc<Bal>) -> Self {
        self.bal_state.set_bal(Some(bal));
        self
    }

    /// Enables BAL building.
    #[inline]
    pub fn with_bal_builder(mut self) -> Self {
        self.bal_state.enable_bal_builder();
        self
    }

    #[inline]
    fn store_bal_error(&mut self, error: BalError) -> DbErrorCode {
        self.bal_error = Some(error);
        bal_error_code()
    }
}

impl<DB: DynDatabase> DynDatabase for BalDatabase<DB> {
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let mut account = self.db.get_account(address)?;
        self.bal_state
            .account(address, &mut account)
            .map_err(|error| self.store_bal_error(error))?;
        Ok(account)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.db.get_code_by_hash(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        if let Some(value) =
            self.bal_state.storage(address, key).map_err(|error| self.store_bal_error(error))?
        {
            return Ok(value);
        }
        self.db.get_storage(address, key)
    }

    fn get_block_hash(&mut self, number: &Word) -> DbResult<Option<B256>> {
        self.db.get_block_hash(number)
    }

    fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
        if code == bal_error_code()
            && let Some(error) = self.bal_error.take()
        {
            return Box::new(error);
        }
        self.db.error(code)
    }

    fn set_bal(&mut self, bal: Option<Arc<Bal>>) {
        self.bal_state.set_bal(bal);
    }

    fn enable_bal_builder(&mut self) {
        self.bal_state.enable_bal_builder();
    }

    fn reset_bal_index(&mut self) {
        self.bal_state.reset_bal_index();
    }

    fn set_bal_index(&mut self, index: BlockAccessIndex) {
        self.bal_state.set_bal_index(index);
    }

    fn bump_bal_index(&mut self) {
        self.bal_state.bump_bal_index();
    }

    fn take_built_bal(&mut self) -> Option<Bal> {
        self.bal_state.take_built_bal()
    }

    fn take_built_alloy_bal(&mut self) -> Option<AlloyBal> {
        self.bal_state.take_built_alloy_bal()
    }

    fn record_state_changes(&mut self, changes: &StateChanges) {
        self.bal_state.record_state_changes(changes);
        self.db.record_state_changes(changes);
    }
}

impl<DB: DatabaseCommit> DatabaseCommit for BalDatabase<DB> {
    fn commit(&mut self, changes: &StateChanges) {
        self.bal_state.record_state_changes(changes);
        self.db.commit(changes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::{CacheDB, EmptyDB, Tracked};

    #[test]
    fn bal_writes_return_previous_value() {
        let writes = BalWrites::new(vec![
            (BlockAccessIndex::new(1), Word::from(10)),
            (BlockAccessIndex::new(3), Word::from(30)),
        ]);

        assert_eq!(writes.get(BlockAccessIndex::new(0)), None);
        assert_eq!(writes.get(BlockAccessIndex::new(1)), None);
        assert_eq!(writes.get(BlockAccessIndex::new(2)), Some(Word::from(10)));
        assert_eq!(writes.get(BlockAccessIndex::new(3)), Some(Word::from(10)));
        assert_eq!(writes.get(BlockAccessIndex::new(4)), Some(Word::from(30)));
    }

    #[test]
    fn bal_database_overlays_prior_account_and_storage_writes() {
        let address = Address::with_last_byte(0x11);
        let slot = Word::from(1);
        let mut bal = Bal::new();
        let mut changes = StateChanges::default();
        changes.accounts.insert(
            address,
            Tracked {
                original: None,
                current: Some(AccountInfo::default().with_balance(Word::from(7))),
                _non_exhaustive: (),
            },
        );
        changes.storage.insert(
            address,
            StorageChangeSet {
                wipe: false,
                slots: BTreeMap::from([(
                    slot,
                    Tracked { original: Word::ZERO, current: Word::from(9), _non_exhaustive: () },
                )]),
                _non_exhaustive: (),
            },
        );
        bal.push_state_changes(BlockAccessIndex::new(1), &changes);

        let mut db = BalDatabase::new(CacheDB::new(EmptyDB::default())).with_bal(Arc::new(bal));
        db.set_bal_index(BlockAccessIndex::new(2));

        assert_eq!(
            db.get_account(&address).unwrap().map(|account| account.balance),
            Some(Word::from(7))
        );
        assert_eq!(db.get_storage(&address, &slot).unwrap(), Word::from(9));
    }
}
