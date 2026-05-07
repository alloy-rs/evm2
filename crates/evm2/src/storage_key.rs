use crate::interpreter::Word;
use alloy_primitives::{
    Address, B256,
    map::{FbBuildHasher, HashMap, HashSet},
};
use core::hash::{Hash, Hasher};

pub(crate) type StorageKeyMap<V> = HashMap<StorageKey, V, FbBuildHasher<52>>;
pub(crate) type StorageKeySet = HashSet<StorageKey, FbBuildHasher<52>>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StorageKey {
    address: Address,
    key: B256,
}

impl StorageKey {
    #[inline]
    pub(crate) fn new(address: Address, key: Word) -> Self {
        Self { address, key: B256::from(key) }
    }

    #[inline]
    pub(crate) const fn address(self) -> Address {
        self.address
    }

    #[inline]
    pub(crate) const fn key(self) -> Word {
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
