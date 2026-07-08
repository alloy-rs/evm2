//! BAL builder module

use super::{BalError, BlockAccessIndex, writes::BalWrites};
use crate::{
    bytecode::{Bytecode, BytecodeDecodeError},
    evm::state::{AccountChange, AccountInfo, StorageSlot, Tracked},
    interpreter::Word,
};
use alloc::vec::Vec;
use alloy_eip7928::{
    AccountChanges as AlloyAccountChanges, BalanceChange as AlloyBalanceChange,
    CodeChange as AlloyCodeChange, NonceChange as AlloyNonceChange,
    SlotChanges as AlloySlotChanges, StorageChange as AlloyStorageChange,
};
use alloy_primitives::{
    Address, B256, U256,
    map::{U256Map, hash_map::Entry},
};
use core::ops::{Deref, DerefMut};

/// Account BAL structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountBal {
    /// Account info bal.
    pub account_info: AccountInfoBal,
    /// Storage bal.
    pub storage: StorageBal,
}

impl Deref for AccountBal {
    type Target = AccountInfoBal;

    fn deref(&self) -> &Self::Target {
        &self.account_info
    }
}

impl DerefMut for AccountBal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.account_info
    }
}

impl AccountBal {
    /// Populate account from BAL. Return true if account info got changed
    pub fn populate_account_info(
        &self,
        bal_index: BlockAccessIndex,
        account: &mut AccountInfo,
    ) -> bool {
        self.account_info.populate_account_info(bal_index, account)
    }

    /// Extend account from an [`AccountChange`] produced by transaction execution.
    ///
    /// The `original` value of the account info and each storage slot is the value at the start of
    /// the transaction, `current` is the value after execution. Loaded-but-unchanged slots are
    /// recorded as reads (empty writes).
    #[inline]
    pub fn update(&mut self, bal_index: BlockAccessIndex, account: &AccountChange) {
        let original = account.original.clone().unwrap_or_default();
        // A selfdestructed account needs no special-casing: transaction finalization already
        // resolved `current` to the EIP-8246 balance-only remnant or to a removed account, and
        // its destroyed storage writes surface as reads (execution-specs `destroy_storage`)
        // through the preserved-reads path, not as zero-writes.
        let present = account.current.clone().unwrap_or_default();
        self.account_info.update(bal_index, &original, &present);

        self.storage.update(bal_index, &account.storage);
    }

    /// Create an account BAL from EIP-7928 [`AlloyAccountChanges`].
    ///
    /// # Errors
    ///
    /// Returns [`BytecodeDecodeError`] if any code change contains bytecode rejected by
    /// [`Bytecode::new_raw_checked`]. This currently happens for malformed EIP-7702
    /// bytecode, such as bytes with the EIP-7702 magic prefix but an invalid length or
    /// unsupported version.
    #[inline]
    pub fn try_from_alloy(
        alloy_account: AlloyAccountChanges,
    ) -> Result<(Address, Self), BytecodeDecodeError> {
        Ok((
            alloy_account.address,
            Self {
                account_info: AccountInfoBal {
                    nonce: BalWrites::from(alloy_account.nonce_changes),
                    balance: BalWrites::from(alloy_account.balance_changes),
                    code: BalWrites::try_from(alloy_account.code_changes.as_slice())?,
                },
                storage: StorageBal::from_iter(
                    alloy_account
                        .storage_changes
                        .into_iter()
                        .chain(
                            alloy_account
                                .storage_reads
                                .into_iter()
                                .map(|key| AlloySlotChanges::new(key, Default::default())),
                        )
                        .map(|slot| (slot.slot, BalWrites::from(slot.changes))),
                ),
            },
        ))
    }

    /// Clone an account BAL from EIP-7928 [`AlloyAccountChanges`] without consuming the source.
    ///
    /// # Errors
    ///
    /// Returns [`BytecodeDecodeError`] if any code change contains bytecode rejected by
    /// [`Bytecode::new_raw_checked`]. This currently happens for malformed EIP-7702
    /// bytecode, such as bytes with the EIP-7702 magic prefix but an invalid length or
    /// unsupported version.
    #[inline]
    pub fn clone_from_alloy(
        alloy_account: &AlloyAccountChanges,
    ) -> Result<(Address, Self), BytecodeDecodeError> {
        Ok((
            alloy_account.address,
            Self {
                account_info: AccountInfoBal {
                    nonce: BalWrites::from(alloy_account.nonce_changes.as_slice()),
                    balance: BalWrites::from(alloy_account.balance_changes.as_slice()),
                    code: BalWrites::try_from(alloy_account.code_changes.as_slice())?,
                },
                storage: StorageBal::from_iter(
                    alloy_account
                        .storage_changes
                        .iter()
                        .map(|slot| (slot.slot, BalWrites::from(slot.changes.as_slice())))
                        .chain(
                            alloy_account
                                .storage_reads
                                .iter()
                                .map(|key| (*key, BalWrites::default())),
                        ),
                ),
            },
        ))
    }

    /// Consumes `AccountBal` and converts it into canonical EIP-7928
    /// [`AlloyAccountChanges`].
    ///
    /// The returned account changes are ordered deterministically: storage reads
    /// and storage changes are sorted lexicographically by slot key, changes
    /// within each storage slot are sorted by block access index, and balance,
    /// nonce, and code changes are sorted by block access index.
    ///
    /// This matches the EIP-7928 ordering requirements:
    /// <https://eips.ethereum.org/EIPS/eip-7928#ordering-uniqueness-and-determinism>.
    #[inline]
    pub fn into_alloy_account(self, address: Address) -> AlloyAccountChanges {
        let (storage_reads, writes) = self.storage.into_vecs();
        let storage_changes = writes
            .into_iter()
            .map(|(key, value)| {
                let mut changes = value
                    .writes
                    .into_iter()
                    .map(|(index, value)| AlloyStorageChange::new(index, value))
                    .collect::<Vec<_>>();
                changes.sort_unstable_by_key(|change| change.block_access_index);

                AlloySlotChanges::new(key, changes)
            })
            .collect::<Vec<_>>();

        let mut balance_changes = self
            .account_info
            .balance
            .writes
            .into_iter()
            .map(|(index, value)| AlloyBalanceChange::new(index, value))
            .collect::<Vec<_>>();
        balance_changes.sort_unstable_by_key(|change| change.block_access_index);

        let mut nonce_changes = self
            .account_info
            .nonce
            .writes
            .into_iter()
            .map(|(index, value)| AlloyNonceChange::new(index, value))
            .collect::<Vec<_>>();
        nonce_changes.sort_unstable_by_key(|change| change.block_access_index);

        let mut code_changes = self
            .account_info
            .code
            .writes
            .into_iter()
            .map(|(index, (_, value))| AlloyCodeChange::new(index, value.original_bytes()))
            .collect::<Vec<_>>();
        code_changes.sort_unstable_by_key(|change| change.block_access_index);

        AlloyAccountChanges {
            address,
            storage_changes,
            storage_reads,
            balance_changes,
            nonce_changes,
            code_changes,
        }
    }
}

/// Account info bal structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountInfoBal {
    /// Nonce builder.
    pub nonce: BalWrites<u64>,
    /// Balance builder.
    pub balance: BalWrites<U256>,
    /// Code builder.
    pub code: BalWrites<(B256, Bytecode)>,
}

impl AccountInfoBal {
    /// Populate account info from BAL. Return true if account info got changed
    pub fn populate_account_info(
        &self,
        bal_index: BlockAccessIndex,
        account: &mut AccountInfo,
    ) -> bool {
        let mut changed = false;
        if let Some(nonce) = self.nonce.get(bal_index) {
            account.nonce = *nonce;
            changed = true;
        }
        if let Some(balance) = self.balance.get(bal_index) {
            account.balance = *balance;
            changed = true;
        }
        if let Some((code_hash, code)) = self.code.get(bal_index) {
            account.code_hash = *code_hash;
            account.code = Some(code.clone());
            changed = true;
        }
        changed
    }

    /// Extend account info from another account info.
    #[inline]
    pub fn update(
        &mut self,
        index: BlockAccessIndex,
        original: &AccountInfo,
        present: &AccountInfo,
    ) {
        self.nonce.update(index, &original.nonce, present.nonce);
        self.balance.update(index, &original.balance, present.balance);
        if original.code_hash != present.code_hash {
            self.code.update_with_key(
                index,
                &original.code_hash,
                (present.code_hash, present.code.clone().unwrap_or_default()),
                |i| &i.0,
            );
        }
    }

    /// Extend account info from another account info.
    #[inline]
    pub fn extend(&mut self, bal_account: Self) {
        self.nonce.extend(bal_account.nonce);
        self.balance.extend(bal_account.balance);
        self.code.extend(bal_account.code);
    }

    /// Update account balance in BAL.
    #[inline]
    pub fn balance_update(
        &mut self,
        bal_index: BlockAccessIndex,
        original_balance: &U256,
        balance: U256,
    ) {
        self.balance.update(bal_index, original_balance, balance);
    }

    /// Update account nonce in BAL.
    #[inline]
    pub fn nonce_update(&mut self, bal_index: BlockAccessIndex, original_nonce: &u64, nonce: u64) {
        self.nonce.update(bal_index, original_nonce, nonce);
    }

    /// Update account code in BAL.
    #[inline]
    pub fn code_update(
        &mut self,
        bal_index: BlockAccessIndex,
        original_code_hash: &B256,
        code_hash: B256,
        code: Bytecode,
    ) {
        self.code.update_with_key(bal_index, original_code_hash, (code_hash, code), |i| &i.0);
    }
}

/// Storage BAL
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StorageBal {
    /// Storage with writes and reads.
    pub storage: U256Map<BalWrites<U256>>,
}

impl StorageBal {
    /// Get storage from the builder.
    #[inline]
    pub fn get(
        &self,
        address: &Address,
        key: U256,
        bal_index: BlockAccessIndex,
    ) -> Result<Option<U256>, BalError> {
        Ok(self.get_bal_writes(address, key)?.get(bal_index).copied())
    }

    /// Get storage writes from the builder.
    ///
    /// `address` is only needed in case of an error to propagate the address.
    #[inline]
    pub fn get_bal_writes(
        &self,
        address: &Address,
        key: U256,
    ) -> Result<&BalWrites<U256>, BalError> {
        self.storage.get(&key).ok_or(BalError::SlotNotFound { address: *address, slot: key })
    }

    /// Extend storage from another storage.
    #[inline]
    pub fn extend(&mut self, storage: Self) {
        self.storage.reserve(storage.storage.len());
        for (key, value) in storage.storage {
            match self.storage.entry(key) {
                Entry::Occupied(mut entry) => {
                    entry.get_mut().extend(value);
                }
                Entry::Vacant(entry) => {
                    entry.insert(value);
                }
            }
        }
    }

    /// Update storage from the per-account storage of an [`AccountChange`].
    #[inline]
    pub fn update(&mut self, bal_index: BlockAccessIndex, storage: &U256Map<Tracked<Word>>) {
        self.storage.reserve(storage.len());
        for (key, value) in storage {
            self.storage.entry(*key).or_default().update(bal_index, &value.original, value.current);
        }
    }

    /// Update storage from an account's pending [`StorageSlot`] overlay: a changed slot records a
    /// write at `bal_index`, a loaded-but-unchanged slot records a read.
    #[inline]
    pub fn update_pending(&mut self, bal_index: BlockAccessIndex, slots: &U256Map<StorageSlot>) {
        self.storage.reserve(slots.len());
        for (key, slot) in slots {
            self.storage.entry(*key).or_default().update(
                bal_index,
                &slot.value.original,
                slot.value.current,
            );
        }
    }

    /// Update reads with new storage keys.
    ///
    /// It will expend inner map with new reads.
    #[inline]
    pub fn update_reads(&mut self, storage: impl Iterator<Item = U256>) {
        for key in storage {
            self.storage.entry(key).or_default();
        }
    }

    /// Insert storage into the builder.
    pub fn extend_iter(&mut self, storage: impl Iterator<Item = (U256, BalWrites<U256>)>) {
        for (key, value) in storage {
            self.storage.insert(key, value);
        }
    }

    /// Convert the storage into a vector of reads and writes, each sorted by slot key.
    pub fn into_vecs(self) -> (Vec<U256>, Vec<(U256, BalWrites<U256>)>) {
        let len = self.storage.len();
        let mut reads = Vec::with_capacity(len);
        let mut writes = Vec::with_capacity(len);

        for (key, value) in self.storage {
            if value.writes.is_empty() {
                reads.push(key);
            } else {
                writes.push((key, value));
            }
        }

        reads.sort_unstable();
        writes.sort_unstable_by_key(|&(key, _)| key);

        (reads, writes)
    }
}

impl FromIterator<(U256, BalWrites<U256>)> for StorageBal {
    fn from_iter<I: IntoIterator<Item = (U256, BalWrites<U256>)>>(iter: I) -> Self {
        Self { storage: iter.into_iter().collect() }
    }
}
