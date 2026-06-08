//! Block-level state accumulation.

use super::{
    AccountChangeRef, AccountInfo, AccountInfoRef, StateChangeSink, StateChangeSource,
    StorageChange, Tracked,
};
use crate::{
    bytecode::Bytecode,
    interpreter::Word,
    storage_key::{StorageKey, StorageKeyMap},
};
use alloc::vec::Vec;
use alloy_primitives::{
    Address, B256,
    map::{AddressMap, AddressSet, B256Map, hash_map},
};
use core::convert::Infallible;

/// Mutable block-level state accumulator.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlockStateAccumulator {
    accounts: AddressMap<Tracked<Option<AccountInfo>>>,
    storage_wipes: AddressSet,
    storage: StorageKeyMap<Tracked<Word>>,
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
        self.accounts.is_empty()
            && self.storage_wipes.is_empty()
            && self.storage.is_empty()
            && self.code.is_empty()
    }

    /// Returns account deltas with addresses in arbitrary map order.
    #[inline]
    pub fn accounts(&self) -> impl Iterator<Item = (Address, &Tracked<Option<AccountInfo>>)> {
        self.accounts.iter().map(|(&address, delta)| (address, delta))
    }

    /// Returns storage-wipe addresses in arbitrary set order.
    #[inline]
    pub fn storage_wipes(&self) -> impl Iterator<Item = Address> + '_ {
        self.storage_wipes.iter().copied()
    }

    /// Returns storage deltas with storage keys in arbitrary map order.
    ///
    /// If a slot's address appears in [`Self::storage_wipes`], consumers should apply the wipe
    /// before this slot and treat [`Tracked::current`] as the value to write after the wipe.
    #[inline]
    pub fn storage(&self) -> impl Iterator<Item = (StorageKey, &Tracked<Word>)> {
        self.storage.iter().map(|(&key, delta)| (key, delta))
    }

    /// Returns bytecode entries in arbitrary map order.
    #[inline]
    pub fn code(&self) -> impl Iterator<Item = (&B256, &Bytecode)> {
        self.code.iter()
    }

    /// Returns account deltas with addresses sorted by address.
    pub fn accounts_sorted(&self) -> Vec<(Address, &Tracked<Option<AccountInfo>>)> {
        let mut accounts = self.accounts().collect::<Vec<_>>();
        accounts.sort_by_key(|(address, _)| *address);
        accounts
    }

    /// Returns storage-wipe addresses sorted by address.
    pub fn storage_wipes_sorted(&self) -> Vec<Address> {
        let mut storage_wipes = self.storage_wipes().collect::<Vec<_>>();
        storage_wipes.sort_unstable();
        storage_wipes
    }

    /// Returns storage deltas with storage keys sorted by address and slot.
    pub fn storage_sorted(&self) -> Vec<(StorageKey, &Tracked<Word>)> {
        let mut storage = self.storage().collect::<Vec<_>>();
        storage.sort_by_key(|(key, _)| (key.address(), key.key()));
        storage
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
                delta.set_current(current);
                if !delta.is_changed() {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if original != current {
                    entry.insert(Tracked::from_parts(original, current));
                }
            }
        }

        if deletes_account {
            self.storage_wipes.remove(&change.address);
            self.storage.retain(|key, _| key.address() != change.address);
        } else if self.accounts.get(&change.address).is_some_and(|delta| delta.original().is_none())
        {
            self.storage_wipes.remove(&change.address);
        }
        Ok(())
    }

    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        let record_wipe =
            self.accounts.get(&address).is_none_or(|delta| delta.original().is_some());
        if record_wipe {
            self.storage_wipes.insert(address);
        }
        self.storage.retain(|key, _| key.address() != address);
        Ok(())
    }

    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        let storage_key = StorageKey::new(change.address, change.key);
        let storage_wiped = self.storage_wipes.contains(&change.address);
        match self.storage.entry(storage_key) {
            hash_map::Entry::Occupied(mut entry) => {
                let delta = entry.get_mut();
                delta.set_current(change.current);
                if (storage_wiped && delta.current().is_zero())
                    || (!storage_wiped && !delta.is_changed())
                {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if (storage_wiped && change.current.is_zero())
                    || (!storage_wiped && change.original == change.current)
                {
                    return Ok(());
                }
                entry.insert(Tracked::from_parts(change.original, change.current));
            }
        }
        Ok(())
    }
}

impl StateChangeSource for BlockStateAccumulator {
    #[inline]
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        visit_block_changes(&self.accounts, &self.storage_wipes, &self.storage, &self.code, sink)
    }
}

fn visit_block_changes<S: StateChangeSink>(
    accounts: &AddressMap<Tracked<Option<AccountInfo>>>,
    storage_wipes: &AddressSet,
    storage: &StorageKeyMap<Tracked<Word>>,
    code: &B256Map<Bytecode>,
    sink: &mut S,
) -> Result<(), S::Error> {
    let mut code_entries = code.iter().collect::<Vec<_>>();
    code_entries.sort_by_key(|(code_hash, _)| **code_hash);
    for (&code_hash, code) in code_entries {
        sink.bytecode(code_hash, code)?;
    }

    let mut storage_wipes = storage_wipes.iter().copied().collect::<Vec<_>>();
    storage_wipes.sort_unstable();
    for address in &storage_wipes {
        sink.storage_wipe(*address)?;
    }

    let mut storage_deltas = storage.iter().collect::<Vec<_>>();
    storage_deltas.sort_by_key(|entry| (entry.0.address(), entry.0.key()));
    for (key, delta) in storage_deltas {
        sink.storage(StorageChange {
            address: key.address(),
            key: key.key(),
            original: *delta.original(),
            current: *delta.current(),
        })?;
    }

    let mut account_deltas = accounts.iter().collect::<Vec<_>>();
    account_deltas.sort_by_key(|entry| *entry.0);
    for (address, delta) in account_deltas {
        sink.account(AccountChangeRef {
            address: *address,
            original: delta.original().as_ref().map(AccountInfoRef::from_info),
            current: delta.current().as_ref().map(AccountInfoRef::from_info),
        })?;
    }
    Ok(())
}
