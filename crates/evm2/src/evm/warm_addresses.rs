//! Warm-address tracking for the transaction-initial (base) warm set.
//!
//! [`WarmAddresses`] stores the addresses that are warm-loaded before EVM execution begins:
//! precompiles, the coinbase/beneficiary (EIP-3651), and the EIP-2930 access list. It is a port of
//! revm's `WarmAddresses`, adapted to evm2's primitive types. Runtime warmth introduced while
//! executing EVM code is tracked separately, per account, by [`crate::evm::State`].

use crate::interpreter::Word;
use alloy_primitives::{
    Address,
    map::{AddressMap, AddressSet, HashSet},
};
use bitvec::{bitvec, vec::BitVec};

/// Short-address optimization cap. Addresses with 18 leading zero bytes whose last two bytes are
/// less than this value are tracked in a bit vector for fast warm lookups.
pub const SHORT_ADDRESS_CAP: usize = 300;

/// Returns the short-address index for an address, if it qualifies.
///
/// A short address has 18 leading zero bytes and a last-two-byte value below [`SHORT_ADDRESS_CAP`].
#[inline]
fn short_address(address: &Address) -> Option<usize> {
    let (zeros, value) = address.split_at(18);
    if zeros.iter().all(|b| *b == 0) {
        let short_address = u16::from_be_bytes([value[0], value[1]]) as usize;
        if short_address < SHORT_ADDRESS_CAP {
            return Some(short_address);
        }
    }
    None
}

/// Stores addresses that are warm-loaded for the current transaction.
///
/// Contains the precompile addresses (which change infrequently), the coinbase address, and the
/// EIP-2930 access list (which changes per transaction). Precompiles are usually very small
/// addresses, so they are additionally stored in `precompile_short_addresses` as a bit vector for
/// faster access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmAddresses {
    /// Set of warm-loaded precompile addresses.
    precompile_set: AddressSet,
    /// Bit vector of precompile short addresses. If an address is shorter than
    /// [`SHORT_ADDRESS_CAP`] it is stored here for faster access.
    precompile_short_addresses: BitVec,
    /// `true` if all precompiles are short addresses.
    precompile_all_short_addresses: bool,
    /// Coinbase address.
    coinbase: Option<Address>,
    /// EIP-2930 access list keyed by address, each holding its warm storage slots.
    access_list: AddressMap<HashSet<Word>>,
}

impl Default for WarmAddresses {
    fn default() -> Self {
        Self::new()
    }
}

impl WarmAddresses {
    /// Creates a new, empty warm-address set.
    #[inline]
    pub fn new() -> Self {
        Self {
            precompile_set: AddressSet::default(),
            precompile_short_addresses: bitvec![0; SHORT_ADDRESS_CAP],
            precompile_all_short_addresses: true,
            coinbase: None,
            access_list: AddressMap::default(),
        }
    }

    /// Returns the precompile addresses.
    #[inline]
    pub const fn precompiles(&self) -> &AddressSet {
        &self.precompile_set
    }

    /// Returns the coinbase address.
    #[inline]
    pub const fn coinbase(&self) -> Option<Address> {
        self.coinbase
    }

    /// Sets the precompile addresses and rebuilds the short-address bit vector.
    pub fn set_precompile_addresses(&mut self, addresses: &AddressSet) {
        self.precompile_short_addresses.fill(false);

        let mut all_short_addresses = true;
        for address in addresses.iter() {
            if let Some(short_address) = short_address(address) {
                self.precompile_short_addresses.set(short_address, true);
            } else {
                all_short_addresses = false;
            }
        }

        self.precompile_all_short_addresses = all_short_addresses;
        self.precompile_set.clone_from(addresses);
    }

    /// Sets the coinbase address.
    #[inline]
    pub const fn set_coinbase(&mut self, address: Address) {
        self.coinbase = Some(address);
    }

    /// Sets the access list.
    #[inline]
    pub fn set_access_list(&mut self, access_list: AddressMap<HashSet<Word>>) {
        self.access_list = access_list;
    }

    /// Returns the access list.
    #[inline]
    pub const fn access_list(&self) -> &AddressMap<HashSet<Word>> {
        &self.access_list
    }

    /// Clears the coinbase address.
    #[inline]
    pub const fn clear_coinbase(&mut self) {
        self.coinbase = None;
    }

    /// Clears the coinbase address and the access list, leaving precompiles intact.
    #[inline]
    pub fn clear_coinbase_and_access_list(&mut self) {
        self.coinbase = None;
        self.access_list.clear();
    }

    /// Returns whether the address is warm-loaded in the base set.
    pub fn is_warm(&self, address: &Address) -> bool {
        // check if it is coinbase
        if Some(*address) == self.coinbase {
            return true;
        }

        // if it is part of the access list.
        if self.access_list.contains_key(address) {
            return true;
        }

        // if there are no precompiles, it is cold-loaded and the bitvec is not set.
        if self.precompile_set.is_empty() {
            return false;
        }

        // check if it is a short precompile address
        if let Some(short_address) = short_address(address) {
            return self.precompile_short_addresses[short_address];
        }

        if !self.precompile_all_short_addresses {
            // otherwise check the precompile set directly
            return self.precompile_set.contains(address);
        }

        false
    }

    /// Returns whether the storage slot is warm-loaded by the access list.
    #[inline]
    pub fn is_storage_warm(&self, address: &Address, key: &Word) -> bool {
        if let Some(access_list) = self.access_list.get(address) {
            return access_list.contains(key);
        }

        false
    }

    /// Returns whether the address is cold-loaded in the base set.
    #[inline]
    pub fn is_cold(&self, address: &Address) -> bool {
        !self.is_warm(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn test_initialization() {
        let warm_addresses = WarmAddresses::new();
        assert!(warm_addresses.precompile_set.is_empty());
        assert_eq!(warm_addresses.precompile_short_addresses.len(), SHORT_ADDRESS_CAP);
        assert!(!warm_addresses.precompile_short_addresses.any());
        assert!(warm_addresses.coinbase.is_none());

        let default_addresses = WarmAddresses::default();
        assert_eq!(warm_addresses, default_addresses);
    }

    #[test]
    fn test_coinbase_management() {
        let mut warm_addresses = WarmAddresses::new();
        let coinbase_addr = address!("1234567890123456789012345678901234567890");

        warm_addresses.set_coinbase(coinbase_addr);
        assert_eq!(warm_addresses.coinbase, Some(coinbase_addr));
        assert!(warm_addresses.is_warm(&coinbase_addr));

        warm_addresses.clear_coinbase_and_access_list();
        assert!(warm_addresses.coinbase.is_none());
        assert!(!warm_addresses.is_warm(&coinbase_addr));
    }

    #[test]
    fn test_short_address_precompiles() {
        let mut warm_addresses = WarmAddresses::new();

        let short_addr1 = Address::with_last_byte(1);
        let short_addr2 = Address::with_last_byte(5);

        let mut precompiles = AddressSet::default();
        precompiles.insert(short_addr1);
        precompiles.insert(short_addr2);

        warm_addresses.set_precompile_addresses(&precompiles);

        assert_eq!(warm_addresses.precompile_set, precompiles);
        assert!(warm_addresses.precompile_short_addresses[1]);
        assert!(warm_addresses.precompile_short_addresses[5]);
        assert!(!warm_addresses.precompile_short_addresses[0]);

        assert!(warm_addresses.is_warm(&short_addr1));
        assert!(warm_addresses.is_warm(&short_addr2));
        assert!(!warm_addresses.is_warm(&Address::with_last_byte(20)));
    }

    #[test]
    fn test_regular_address_precompiles() {
        let mut warm_addresses = WarmAddresses::new();

        let regular_addr = address!("1234567890123456789012345678901234567890");
        let mut bytes = [0u8; 20];
        bytes[18] = 1u8;
        bytes[19] = 44u8; // 300
        let boundary_addr = Address::from(bytes);

        let mut precompiles = AddressSet::default();
        precompiles.insert(regular_addr);
        precompiles.insert(boundary_addr);

        warm_addresses.set_precompile_addresses(&precompiles);

        assert_eq!(warm_addresses.precompile_set, precompiles);
        assert!(!warm_addresses.precompile_short_addresses.any());

        assert!(warm_addresses.is_warm(&regular_addr));
        assert!(warm_addresses.is_warm(&boundary_addr));
        assert!(!warm_addresses.is_warm(&address!("0987654321098765432109876543210987654321")));
    }

    #[test]
    fn test_storage_warm_via_access_list() {
        let mut warm_addresses = WarmAddresses::new();
        let addr = address!("1234567890123456789012345678901234567890");
        let key = Word::from(7);

        let mut slots = HashSet::default();
        slots.insert(key);
        let mut access_list = AddressMap::default();
        access_list.insert(addr, slots);
        warm_addresses.set_access_list(access_list);

        assert!(warm_addresses.is_warm(&addr));
        assert!(warm_addresses.is_storage_warm(&addr, &key));
        assert!(!warm_addresses.is_storage_warm(&addr, &Word::from(8)));
    }
}
