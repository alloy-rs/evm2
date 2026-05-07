use crate::interpreter::Word;
use alloy_primitives::{
    Address, B256,
    map::{FbBuildHasher, HashMap, HashSet},
};
use core::hash::{Hash, Hasher};

/// Hash map keyed by storage account and slot.
pub type StorageKeyMap<V> = HashMap<StorageKey, V, FbBuildHasher<52>>;

/// Hash set keyed by storage account and slot.
pub type StorageKeySet = HashSet<StorageKey, FbBuildHasher<52>>;

/// Storage key for account-address and storage-slot pairs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageKey {
    address: Address,
    key: B256,
}

impl StorageKey {
    /// Creates a storage key from an account address and slot.
    #[inline]
    pub fn new(address: Address, key: Word) -> Self {
        Self { address, key: B256::from(key) }
    }

    /// Returns the account address.
    #[inline]
    pub const fn address(self) -> Address {
        self.address
    }

    /// Returns the storage slot.
    #[inline]
    pub const fn key(self) -> Word {
        Word::from_be_bytes(self.key.0)
    }
}

impl Hash for StorageKey {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut bytes = [0; 52];
        bytes[..20].copy_from_slice(self.address.as_slice());
        bytes[20..].copy_from_slice(self.key.as_slice());
        state.write(&bytes);
    }
}
