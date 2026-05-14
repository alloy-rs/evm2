//! Block access list construction from transaction state changes.

use super::{AccountInfo, StateChanges};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use alloy_eip7928::{
    AccountChanges, BalanceChange, BlockAccessIndex, BlockAccessList, CodeChange, NonceChange,
    SlotChanges, StorageChange,
};
use alloy_primitives::{Address, Bytes};

/// Builder for EIP-7928 block access lists.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BalBuilder {
    accounts: BTreeMap<Address, AccountBalBuilder>,
}

impl BalBuilder {
    /// Pushes a transaction or system-call state transition into the block access list.
    pub fn push_state_changes(&mut self, index: BlockAccessIndex, changes: &StateChanges) {
        for (&address, account) in &changes.accounts {
            let builder = self.accounts.entry(address).or_default();
            let current = account.current.as_ref();
            let empty = AccountInfo::default();
            let post = current.unwrap_or(&empty);
            let original = account.original.as_ref();

            if original.map_or(!post.balance.is_zero(), |original| original.balance != post.balance)
            {
                builder.balance_changes.push(BalanceChange::new(index, post.balance));
            }
            if original.map_or(post.nonce != 0, |original| original.nonce != post.nonce) {
                builder.nonce_changes.push(NonceChange::new(index, post.nonce));
            }
            if original.map_or(post.code_hash != alloy_primitives::KECCAK256_EMPTY, |original| {
                original.code_hash != post.code_hash
            }) {
                builder.code_changes.push(CodeChange::new(index, code_bytes(current)));
            }
        }

        for (&address, storage) in &changes.storage {
            let builder = self.accounts.entry(address).or_default();
            for (&slot, change) in &storage.slots {
                builder.push_storage_change(index, slot, change.current);
            }
        }

        if let Some(accesses) = &changes.accesses {
            for (&address, slots) in &accesses.storage {
                let changed_slots = changes.storage.get(&address);
                let builder = self.accounts.entry(address).or_default();
                for &slot in slots {
                    let slot_was_written =
                        changed_slots.is_some_and(|storage| storage.slots.contains_key(&slot));
                    if slot_was_written {
                        continue;
                    }
                    if changed_slots.is_some_and(|storage| storage.wipe) {
                        builder.push_storage_change(index, slot, Word::ZERO);
                    } else if !builder.storage_changes.contains_key(&slot) {
                        builder.storage_reads.insert(slot);
                    }
                }
            }
        }
    }

    /// Consumes the builder and returns a canonical EIP-7928 block access list.
    pub fn build(self) -> BlockAccessList {
        self.accounts.into_iter().filter_map(|(address, builder)| builder.build(address)).collect()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AccountBalBuilder {
    storage_reads: BTreeSet<Word>,
    storage_changes: BTreeMap<Word, Vec<StorageChange>>,
    balance_changes: Vec<BalanceChange>,
    nonce_changes: Vec<NonceChange>,
    code_changes: Vec<CodeChange>,
}

impl AccountBalBuilder {
    fn push_storage_change(&mut self, index: BlockAccessIndex, slot: Word, value: Word) {
        self.storage_reads.remove(&slot);
        self.storage_changes.entry(slot).or_default().push(StorageChange::new(index, value));
    }

    fn build(self, address: Address) -> Option<AccountChanges> {
        if self.storage_reads.is_empty()
            && self.storage_changes.is_empty()
            && self.balance_changes.is_empty()
            && self.nonce_changes.is_empty()
            && self.code_changes.is_empty()
        {
            return None;
        }

        let mut account = AccountChanges {
            address,
            storage_changes: self
                .storage_changes
                .into_iter()
                .map(|(slot, changes)| SlotChanges::new(slot, changes))
                .collect(),
            storage_reads: self.storage_reads.into_iter().collect(),
            balance_changes: self.balance_changes,
            nonce_changes: self.nonce_changes,
            code_changes: self.code_changes,
        };
        account.sort();
        Some(account)
    }
}

fn code_bytes(account: Option<&AccountInfo>) -> Bytes {
    account.and_then(|info| info.code.as_ref()).map(Bytecode::original_bytes).unwrap_or_default()
}
