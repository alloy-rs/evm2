//! Block-level state accumulation and frozen views.

use super::{
    AccountChangeRef, AccountInfo, AccountInfoRef, StateChangeSink, StateChangeSource,
    StorageChangeRef,
};
use crate::{
    bytecode::Bytecode,
    interpreter::Word,
    storage_key::{StorageKey, StorageKeyMap},
};
use alloc::vec::Vec;
use alloy_primitives::{
    Address, B256,
    map::{AddressMap, B256Map, hash_map},
};
use core::convert::Infallible;

/// Block-level account delta accumulated from committed transactions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockAccountDelta {
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
    /// Slot value from the first observed non-wipe change.
    ///
    /// If the account's [`BlockAccountDelta::storage_wiped`] flag is set, consumers should apply
    /// the wipe before this slot and treat `current` as the value to write after the wipe.
    pub original: Word,
    /// Slot value after the latest committed transaction.
    pub current: Word,
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

impl StateChangeSink for BlockStateAccumulator {
    type Error = Infallible;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.code.entry(code_hash).or_insert_with(|| code.clone());
        Ok(())
    }

    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        let original = change.original.map(AccountInfoRef::to_account_info_without_code);
        let current = change.current.map(AccountInfoRef::to_account_info_without_code);
        let deletes_account = current.is_none();
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
                    entry.insert(BlockAccountDelta { original, current, storage_wiped: false });
                }
            }
        }
        if deletes_account {
            self.storage.retain(|key, _| key.address() != change.address);
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
                    original: None,
                    current: None,
                    storage_wiped: true,
                });
        }

        self.storage.retain(|key, _| key.address() != address);
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
                if (after_wipe && delta.current.is_zero())
                    || (!after_wipe && delta.original == delta.current)
                {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if (after_wipe && change.current.is_zero())
                    || (!after_wipe && change.original == change.current)
                {
                    return Ok(());
                }
                entry.insert(BlockStorageDelta {
                    original: change.original,
                    current: change.current,
                });
            }
        }
        Ok(())
    }
}

impl StateChangeSource for BlockStateAccumulator {
    #[inline]
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        visit_block_changes(&self.accounts, &self.storage, &self.code, sink)
    }
}

fn visit_block_changes<S: StateChangeSink>(
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

    let mut account_deltas = accounts.iter().collect::<Vec<_>>();
    account_deltas.sort_by_key(|entry| *entry.0);
    for (address, delta) in &account_deltas {
        if delta.storage_wiped {
            sink.storage_wipe(**address)?;
        }
    }

    let mut storage_deltas = storage.iter().collect::<Vec<_>>();
    storage_deltas.sort_by_key(|entry| (entry.0.address(), entry.0.key()));
    for (key, delta) in storage_deltas {
        let address = key.address();
        sink.storage(StorageChangeRef {
            address,
            key: key.key(),
            original: delta.original,
            current: delta.current,
            after_wipe: accounts.get(&address).is_some_and(|delta| delta.storage_wiped),
        })?;
    }

    for (address, delta) in account_deltas {
        if delta.original != delta.current {
            sink.account(AccountChangeRef {
                address: *address,
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
    /// Returns account deltas with addresses in arbitrary map order.
    #[inline]
    pub fn accounts(&self) -> impl Iterator<Item = (Address, &BlockAccountDelta)> {
        self.accounts.iter().map(|(&address, delta)| (address, delta))
    }

    /// Returns storage deltas with storage keys in arbitrary map order.
    #[inline]
    pub fn storage(&self) -> impl Iterator<Item = (StorageKey, &BlockStorageDelta)> {
        self.storage.iter().map(|(&key, delta)| (key, delta))
    }

    /// Returns bytecode entries in arbitrary map order.
    #[inline]
    pub fn code(&self) -> impl Iterator<Item = (&B256, &Bytecode)> {
        self.code.iter()
    }

    /// Returns account deltas with addresses sorted by address.
    pub fn accounts_sorted(&self) -> Vec<(Address, &BlockAccountDelta)> {
        let mut accounts = self.accounts().collect::<Vec<_>>();
        accounts.sort_by_key(|(address, _)| *address);
        accounts
    }

    /// Returns storage deltas with storage keys sorted by address and slot.
    pub fn storage_sorted(&self) -> Vec<(StorageKey, &BlockStorageDelta)> {
        let mut storage = self.storage().collect::<Vec<_>>();
        storage.sort_by_key(|(key, _)| (key.address(), key.key()));
        storage
    }
}

impl StateChangeSource for FrozenBlockState {
    #[inline]
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        visit_block_changes(&self.accounts, &self.storage, &self.code, sink)
    }
}
