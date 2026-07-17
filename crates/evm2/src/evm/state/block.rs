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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
                // Reviving an account deleted earlier in the block re-records the deletion's
                // implied storage wipe: the deletion dropped the wipe marker because applying
                // `current == None` wipes storage at the sink, but applying a live account does
                // not, so pre-block storage would otherwise leak through the revival.
                if delta.current.is_none() && current.is_some() && delta.original.is_some() {
                    self.storage_wipes.insert(change.address);
                }
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
        } else if self.accounts.get(&change.address).is_some_and(|delta| delta.original.is_none()) {
            self.storage_wipes.remove(&change.address);
        }
        Ok(())
    }

    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        let record_wipe = self.accounts.get(&address).is_none_or(|delta| delta.original.is_some());
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
                if (storage_wiped && delta.current.is_zero())
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
            original: delta.original,
            current: delta.current,
        })?;
    }

    let mut account_deltas = accounts.iter().collect::<Vec<_>>();
    account_deltas.sort_by_key(|entry| *entry.0);
    for (address, delta) in account_deltas {
        sink.account(AccountChangeRef {
            address: *address,
            original: delta.original.as_ref().map(AccountInfoRef::from_info),
            current: delta.current.as_ref().map(AccountInfoRef::from_info),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::{AccountChange, AccountInfo, StateChangeSource, StateChanges, Tracked},
        BlockStateAccumulator,
    };
    use crate::interpreter::Word;
    use alloy_primitives::{Address, map::U256Map};

    #[cfg(feature = "serde")]
    use super::super::StateChangeSink;
    #[cfg(feature = "serde")]
    use crate::bytecode::Bytecode;
    #[cfg(feature = "serde")]
    use alloc::vec;
    #[cfg(feature = "serde")]
    use alloy_primitives::B256;

    fn changes(address: Address, change: AccountChange) -> StateChanges {
        let mut changes = StateChanges::default();
        changes.accounts.insert(address, change);
        changes
    }

    fn slot(key: Word, original: Word, current: Word) -> U256Map<Tracked<Word>> {
        U256Map::from_iter([(key, Tracked::from_parts(original, current))])
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_binary_roundtrip() {
        let mut accumulator = BlockStateAccumulator::new();
        let code_hash = B256::with_last_byte(1);
        let bytecode = Bytecode::new_raw_checked(vec![0x60, 0x00].into()).unwrap();
        accumulator.bytecode(code_hash, &bytecode).unwrap();

        let encoded = postcard::to_allocvec(&accumulator).unwrap();
        let deserialized: BlockStateAccumulator = postcard::from_bytes(&encoded).unwrap();

        assert_eq!(deserialized, accumulator);
    }

    #[test]
    fn block_accumulator_collapses_create_then_delete() {
        let address = Address::from([0x50; 20]);
        let key = Word::from(1);
        let created = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        let create = changes(
            address,
            AccountChange {
                current: Some(created.clone()),
                storage: slot(key, Word::ZERO, Word::from(7)),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        create.visit(&mut accumulator).expect("block accumulator is infallible");

        let delete = changes(
            address,
            AccountChange {
                original: Some(created),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        assert!(accumulator.accounts_sorted().is_empty());
        assert!(accumulator.storage_wipes_sorted().is_empty());
        assert!(accumulator.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_preserves_original_for_delete_then_recreate() {
        let address = Address::from([0x51; 20]);
        let key = Word::from(1);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let recreated = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        let delete = changes(
            address,
            AccountChange {
                original: Some(original.clone()),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        let create = changes(
            address,
            AccountChange {
                current: Some(recreated.clone()),
                storage: slot(key, Word::ZERO, Word::from(7)),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        create.visit(&mut accumulator).expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].1.original.as_ref(), Some(&original));
        assert_eq!(accounts[0].1.current.as_ref(), Some(&recreated));
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);

        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.current, Word::from(7));
    }

    #[test]
    fn block_accumulator_restores_wipe_when_deleted_account_is_revived() {
        // Selfdestruct followed by a revival in a later transaction of the same block (a plain
        // transfer or a re-create) must keep the deletion's storage wipe: the revived account is
        // applied as a live account, which does not wipe storage at the sink by itself.
        let address = Address::from([0x55; 20]);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let revived = AccountInfo::default().with_balance(Word::from(1));
        let mut accumulator = BlockStateAccumulator::new();

        let delete = changes(
            address,
            AccountChange {
                original: Some(original.clone()),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        delete.visit(&mut accumulator).expect("block accumulator is infallible");
        assert!(accumulator.storage_wipes_sorted().is_empty(), "deletion subsumes the wipe");

        let revive = changes(
            address,
            AccountChange { current: Some(revived.clone()), ..AccountChange::default() },
        );
        revive.visit(&mut accumulator).expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].1.original.as_ref(), Some(&original));
        assert_eq!(accounts[0].1.current.as_ref(), Some(&revived));
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);

        // Reviving back to a value equal to the original collapses the account delta, but the
        // wipe still stands: the account's pre-block storage was destroyed.
        let mut collapsing = BlockStateAccumulator::new();
        changes(
            address,
            AccountChange {
                original: Some(original.clone()),
                wipe_storage: true,
                ..AccountChange::default()
            },
        )
        .visit(&mut collapsing)
        .expect("block accumulator is infallible");
        changes(address, AccountChange { current: Some(original), ..AccountChange::default() })
            .visit(&mut collapsing)
            .expect("block accumulator is infallible");
        assert!(collapsing.accounts_sorted().is_empty());
        assert_eq!(collapsing.storage_wipes_sorted(), [address]);
    }

    #[test]
    fn block_accumulator_keeps_nonzero_write_after_storage_wipe() {
        let address = Address::from([0x52; 20]);
        let key = Word::from(1);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let mut accumulator = BlockStateAccumulator::new();

        let wipe_and_restore = changes(
            address,
            AccountChange {
                original: Some(original.clone()),
                current: Some(original),
                storage: slot(key, Word::ZERO, Word::from(5)),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        wipe_and_restore.visit(&mut accumulator).expect("block accumulator is infallible");

        assert!(accumulator.accounts_sorted().is_empty());
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);

        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.current, Word::from(5));
    }

    #[test]
    fn block_accumulator_deletion_subsumes_storage_writes() {
        let address = Address::from([0x56; 20]);
        let key = Word::from(1);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let mut accumulator = BlockStateAccumulator::new();

        let delete = changes(
            address,
            AccountChange {
                original: Some(original),
                storage: slot(key, Word::from(5), Word::from(7)),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, address);
        assert!(accounts[0].1.current.is_none());
        assert!(accumulator.storage_wipes_sorted().is_empty());
        assert!(accumulator.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_collapses_storage_wipe_write_wipe() {
        let address = Address::from([0x52; 20]);
        let key = Word::from(1);
        let mut accumulator = BlockStateAccumulator::new();

        let first = changes(
            address,
            AccountChange {
                storage: slot(key, Word::from(5), Word::from(7)),
                wipe_storage: true,
                ..AccountChange::default()
            },
        );
        first.visit(&mut accumulator).expect("block accumulator is infallible");
        changes(address, AccountChange { wipe_storage: true, ..AccountChange::default() })
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert!(accumulator.accounts_sorted().is_empty());
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);
        assert!(accumulator.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_keeps_account_only_and_storage_only_changes_separate() {
        let account_address = Address::from([0x53; 20]);
        let storage_address = Address::from([0x54; 20]);
        let key = Word::from(1);
        let original = AccountInfo::default().with_balance(Word::from(1));
        let current = AccountInfo::default().with_balance(Word::from(2));
        let mut accumulator = BlockStateAccumulator::new();

        changes(
            account_address,
            AccountChange {
                original: Some(original.clone()),
                current: Some(current.clone()),
                ..AccountChange::default()
            },
        )
        .visit(&mut accumulator)
        .expect("block accumulator is infallible");
        changes(
            storage_address,
            AccountChange {
                storage: slot(key, Word::from(3), Word::from(4)),
                ..AccountChange::default()
            },
        )
        .visit(&mut accumulator)
        .expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, account_address);
        assert_eq!(accounts[0].1.original.as_ref(), Some(&original));
        assert_eq!(accounts[0].1.current.as_ref(), Some(&current));
        assert!(accumulator.storage_wipes_sorted().is_empty());

        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.address(), storage_address);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.original, Word::from(3));
        assert_eq!(storage[0].1.current, Word::from(4));
    }
}
