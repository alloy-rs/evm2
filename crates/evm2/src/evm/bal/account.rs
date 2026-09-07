//! BAL builder module

use super::{
    BalChangeKind, BalDecodeError, BalError, BlockAccessIndex,
    changes::{BalChanges, BalCodeChange},
};
use crate::{
    bytecode::Bytecode,
    evm::state::{AccountInfo, StorageOverlay},
};
use alloc::vec::Vec;
use alloy_eip7928::{
    AccountChanges as AlloyAccountChanges, BalanceChange, CodeChange as AlloyCodeChange,
    NonceChange, SlotChanges as AlloySlotChanges, StorageChange,
};
use alloy_primitives::{
    Address, B256, U256,
    map::{U256Map, hash_map::Entry},
};

/// Account BAL structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountBal {
    /// Account info bal.
    pub account_info: AccountInfoBal,
    /// Storage bal.
    pub storage: StorageBal,
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
}

impl TryFrom<&AlloyAccountChanges> for AccountBal {
    type Error = BalDecodeError;

    /// Create an account BAL from borrowed EIP-7928 [`AlloyAccountChanges`] without
    /// consuming the source.
    ///
    /// The account address is not part of the result; read it from
    /// [`AlloyAccountChanges::address`] before converting.
    ///
    /// # Errors
    ///
    /// Returns [`BalDecodeError`] if the account entry violates EIP-7928 ordering or uniqueness,
    /// contains an invalid block access index, or contains malformed bytecode.
    #[inline]
    fn try_from(alloy_account: &AlloyAccountChanges) -> Result<Self, Self::Error> {
        let address = alloy_account.address;
        Ok(Self {
            account_info: AccountInfoBal {
                nonce: checked_clone_changes(
                    address,
                    BalChangeKind::Nonce,
                    &alloy_account.nonce_changes,
                )?,
                balance: checked_clone_changes(
                    address,
                    BalChangeKind::Balance,
                    &alloy_account.balance_changes,
                )?,
                code: checked_code_changes_ref(address, &alloy_account.code_changes)?,
            },
            storage: checked_storage_ref(alloy_account)?,
        })
    }
}

impl From<AccountBal> for AlloyAccountChanges {
    /// Consumes `AccountBal` and converts it into canonical EIP-7928
    /// [`AlloyAccountChanges`].
    ///
    /// The account address is not part of the source; the returned changes carry
    /// [`Address::ZERO`] and the caller is expected to set
    /// [`AlloyAccountChanges::address`].
    ///
    /// The returned account changes are ordered deterministically: storage reads
    /// and storage changes are sorted lexicographically by slot key, changes
    /// within each storage slot are sorted by block access index, and balance,
    /// nonce, and code changes are sorted by block access index.
    ///
    /// This matches the EIP-7928 ordering requirements:
    /// <https://eips.ethereum.org/EIPS/eip-7928#ordering-uniqueness-and-determinism>.
    #[inline]
    fn from(account: AccountBal) -> Self {
        let (storage_reads, writes) = account.storage.into_vecs();
        let storage_changes = writes
            .into_iter()
            .map(|(key, value)| {
                let mut changes = value.changes;
                changes.sort_unstable_by_key(|change| change.block_access_index);

                AlloySlotChanges::new(key, changes)
            })
            .collect::<Vec<_>>();

        let mut balance_changes = account.account_info.balance.changes;
        balance_changes.sort_unstable_by_key(|change| change.block_access_index);

        let mut nonce_changes = account.account_info.nonce.changes;
        nonce_changes.sort_unstable_by_key(|change| change.block_access_index);

        let mut code_changes = account
            .account_info
            .code
            .changes
            .into_iter()
            .map(AlloyCodeChange::from)
            .collect::<Vec<_>>();
        code_changes.sort_unstable_by_key(|change| change.block_access_index);

        Self {
            address: Address::ZERO,
            storage_changes,
            storage_reads,
            balance_changes,
            nonce_changes,
            code_changes,
        }
    }
}

impl TryFrom<AlloyAccountChanges> for AccountBal {
    type Error = BalDecodeError;

    /// Create an account BAL from EIP-7928 [`AlloyAccountChanges`].
    ///
    /// The account address is not part of the result; read it from
    /// [`AlloyAccountChanges::address`] before converting.
    ///
    /// # Errors
    ///
    /// Returns [`BalDecodeError`] if the account entry violates EIP-7928 ordering or uniqueness,
    /// contains an invalid block access index, or contains malformed bytecode.
    #[inline]
    fn try_from(alloy_account: AlloyAccountChanges) -> Result<Self, Self::Error> {
        let address = alloy_account.address;
        Ok(Self {
            account_info: AccountInfoBal {
                nonce: checked_changes(address, BalChangeKind::Nonce, alloy_account.nonce_changes)?,
                balance: checked_changes(
                    address,
                    BalChangeKind::Balance,
                    alloy_account.balance_changes,
                )?,
                code: checked_code_changes(address, alloy_account.code_changes)?,
            },
            storage: checked_storage(
                address,
                alloy_account.storage_changes,
                alloy_account.storage_reads,
            )?,
        })
    }
}

/// Clone and validate borrowed storage entries while building the storage map.
#[inline]
fn checked_storage_ref(account: &AlloyAccountChanges) -> Result<StorageBal, BalDecodeError> {
    let address = account.address;
    let mut storage = U256Map::with_capacity_and_hasher(
        account.storage_changes.len() + account.storage_reads.len(),
        Default::default(),
    );
    let mut previous = None;
    for slot in &account.storage_changes {
        check_storage_key_order(address, &mut previous, slot.slot)?;
        if slot.changes.is_empty() {
            return Err(BalDecodeError::EmptyStorageChanges { address, slot: slot.slot });
        }
        let changes = checked_clone_changes(address, BalChangeKind::Storage, &slot.changes)?;
        if storage.insert(slot.slot, changes).is_some() {
            return Err(BalDecodeError::DuplicateStorageKey { address, slot: slot.slot });
        }
    }
    previous = None;
    for &slot in &account.storage_reads {
        check_storage_key_order(address, &mut previous, slot)?;
        if storage.insert(slot, BalChanges::default()).is_some() {
            return Err(BalDecodeError::DuplicateStorageKey { address, slot });
        }
    }
    Ok(StorageBal { storage })
}

/// Validate owned storage entries while building the storage map.
#[inline]
fn checked_storage(
    address: Address,
    storage_changes: Vec<AlloySlotChanges>,
    storage_reads: Vec<U256>,
) -> Result<StorageBal, BalDecodeError> {
    let mut storage = U256Map::with_capacity_and_hasher(
        storage_changes.len() + storage_reads.len(),
        Default::default(),
    );
    let mut previous = None;
    for slot in storage_changes {
        check_storage_key_order(address, &mut previous, slot.slot)?;
        if slot.changes.is_empty() {
            return Err(BalDecodeError::EmptyStorageChanges { address, slot: slot.slot });
        }
        let changes = checked_changes(address, BalChangeKind::Storage, slot.changes)?;
        if storage.insert(slot.slot, changes).is_some() {
            return Err(BalDecodeError::DuplicateStorageKey { address, slot: slot.slot });
        }
    }
    previous = None;
    for slot in storage_reads {
        check_storage_key_order(address, &mut previous, slot)?;
        if storage.insert(slot, BalChanges::default()).is_some() {
            return Err(BalDecodeError::DuplicateStorageKey { address, slot });
        }
    }
    Ok(StorageBal { storage })
}

/// Enforce strictly increasing storage keys.
#[inline]
fn check_storage_key_order(
    address: Address,
    previous: &mut Option<U256>,
    slot: U256,
) -> Result<(), BalDecodeError> {
    if let Some(previous) = *previous {
        if previous == slot {
            return Err(BalDecodeError::DuplicateStorageKey { address, slot });
        }
        if previous > slot {
            return Err(BalDecodeError::StorageKeysOutOfOrder { address, previous, slot });
        }
    }
    *previous = Some(slot);
    Ok(())
}

/// Clone a borrowed change list while validating its indices.
#[inline]
fn checked_clone_changes<T: super::BalChange + Clone>(
    address: Address,
    kind: BalChangeKind,
    changes: &[T],
) -> Result<BalChanges<T>, BalDecodeError> {
    let mut converted = Vec::with_capacity(changes.len());
    let mut previous = None;
    for change in changes {
        check_change_index(address, kind, &mut previous, change.block_access_index())?;
        converted.push(change.clone());
    }
    Ok(converted.into())
}

/// Validate and wrap an owned change list without reallocating it.
#[inline]
fn checked_changes<T: super::BalChange>(
    address: Address,
    kind: BalChangeKind,
    changes: Vec<T>,
) -> Result<BalChanges<T>, BalDecodeError> {
    let mut previous = None;
    for change in &changes {
        check_change_index(address, kind, &mut previous, change.block_access_index())?;
    }
    Ok(changes.into())
}

/// Decode and validate borrowed code changes in one pass.
#[inline]
fn checked_code_changes_ref(
    address: Address,
    changes: &[AlloyCodeChange],
) -> Result<BalChanges<BalCodeChange>, BalDecodeError> {
    let mut converted = Vec::with_capacity(changes.len());
    let mut previous = None;
    for change in changes {
        check_change_index(address, BalChangeKind::Code, &mut previous, change.block_access_index)?;
        converted.push(BalCodeChange::try_from(change)?);
    }
    Ok(converted.into())
}

/// Decode and validate owned code changes in one pass.
#[inline]
fn checked_code_changes(
    address: Address,
    changes: Vec<AlloyCodeChange>,
) -> Result<BalChanges<BalCodeChange>, BalDecodeError> {
    let mut converted = Vec::with_capacity(changes.len());
    let mut previous = None;
    for change in changes {
        check_change_index(address, BalChangeKind::Code, &mut previous, change.block_access_index)?;
        converted.push(BalCodeChange::try_from(change)?);
    }
    Ok(converted.into())
}

/// Enforce valid, strictly increasing block access indices.
#[inline]
fn check_change_index(
    address: Address,
    kind: BalChangeKind,
    previous: &mut Option<BlockAccessIndex>,
    index: BlockAccessIndex,
) -> Result<(), BalDecodeError> {
    if index.get() > u32::MAX as u64 {
        return Err(BalDecodeError::BlockAccessIndexOutOfRange { address, kind, index });
    }
    if let Some(previous) = *previous {
        if previous == index {
            return Err(BalDecodeError::DuplicateBlockAccessIndex { address, kind, index });
        }
        if previous > index {
            return Err(BalDecodeError::ChangeIndicesOutOfOrder { address, kind, previous, index });
        }
    }
    *previous = Some(index);
    Ok(())
}

/// Account info bal structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountInfoBal {
    /// Nonce builder.
    pub nonce: BalChanges<NonceChange>,
    /// Balance builder.
    pub balance: BalChanges<BalanceChange>,
    /// Code builder.
    pub code: BalChanges<BalCodeChange>,
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
    pub storage: U256Map<BalChanges<StorageChange>>,
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
        Ok(self.get_bal_changes(address, key)?.get(bal_index).copied())
    }

    /// Get storage changes from the builder.
    ///
    /// `address` is only needed in case of an error to propagate the address.
    #[inline]
    pub fn get_bal_changes(
        &self,
        address: &Address,
        key: U256,
    ) -> Result<&BalChanges<StorageChange>, BalError> {
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

    /// Update storage from an account's pending [`StorageOverlay`]: a changed slot records a write
    /// at `bal_index`, and a loaded-but-unchanged slot records a read.
    ///
    /// A wipe converts every storage key previously accessed for the account into a read, as
    /// required for storage within a selfdestructed contract. Wiped slots whose final value is zero
    /// remain reads; any non-zero post-wipe slot is recorded as a subsequent write.
    #[inline]
    pub fn update_pending(&mut self, bal_index: BlockAccessIndex, overlay: &StorageOverlay) {
        if overlay.wiped {
            self.record_wipe();
        }
        self.storage.reserve(overlay.slots.len());
        for (key, slot) in &overlay.slots {
            let changes = self.storage.entry(*key).or_default();
            if !overlay.wiped || !slot.value.current.is_zero() {
                changes.update(bal_index, &slot.value.original, slot.value.current);
            }
        }
    }

    /// Converts all storage keys accumulated so far into reads after a storage wipe.
    #[inline]
    pub(crate) fn record_wipe(&mut self) {
        for changes in self.storage.values_mut() {
            changes.changes.clear();
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
    pub fn extend_iter(
        &mut self,
        storage: impl Iterator<Item = (U256, BalChanges<StorageChange>)>,
    ) {
        for (key, value) in storage {
            self.storage.insert(key, value);
        }
    }

    /// Convert the storage into a vector of reads and writes, each sorted by slot key.
    pub fn into_vecs(self) -> (Vec<U256>, Vec<(U256, BalChanges<StorageChange>)>) {
        let len = self.storage.len();
        let mut reads = Vec::with_capacity(len);
        let mut writes = Vec::with_capacity(len);

        for (key, value) in self.storage {
            if value.is_empty() {
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

impl FromIterator<(U256, BalChanges<StorageChange>)> for StorageBal {
    fn from_iter<I: IntoIterator<Item = (U256, BalChanges<StorageChange>)>>(iter: I) -> Self {
        Self { storage: iter.into_iter().collect() }
    }
}
