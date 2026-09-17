//! Bounded reuse of empty transaction slot maps. No transaction state is retained.

use super::{StorageOverlay, StorageSlot};
use alloc::vec::Vec;
use alloy_primitives::map::{AddressMap, U256Map};

const MAX_MAPS: usize = 32;
const MAX_MAP_CAPACITY: usize = 4_096;
const MAX_TOTAL_CAPACITY: usize = 16_384;

/// Retains only empty allocations. Active transaction maps and the accepted cache are separate.
#[derive(Debug, Default)]
pub(super) struct StoragePool {
    maps: Vec<U256Map<StorageSlot>>,
    capacity: usize,
}

impl StoragePool {
    pub(super) fn take(&mut self) -> U256Map<StorageSlot> {
        match self.maps.pop() {
            Some(map) => {
                self.capacity -= map.capacity();
                map
            }
            None => U256Map::default(),
        }
    }

    /// Called only after the transaction is accepted or discarded, never on a scope rollback.
    /// Draining preserves the outer table allocation while removing all owner identities/wipes.
    pub(super) fn clear(&mut self, storage: &mut AddressMap<StorageOverlay>) {
        for (_, overlay) in storage.drain() {
            let mut slots = overlay.slots;
            let capacity = slots.capacity();
            if capacity == 0
                || capacity > MAX_MAP_CAPACITY
                || self.maps.len() == MAX_MAPS
                || capacity > MAX_TOTAL_CAPACITY - self.capacity
            {
                // Do not spend time clearing a large allocation that will be dropped anyway.
                continue;
            }
            slots.clear();
            // Clearing deletion markers can increase a hash map's reported capacity. Charge the
            // empty allocation we actually retain, not its possibly lower pre-clear capacity.
            let capacity = slots.capacity();
            if capacity > MAX_MAP_CAPACITY || capacity > MAX_TOTAL_CAPACITY - self.capacity {
                continue;
            }
            self.capacity += capacity;
            self.maps.push(slots);
        }
    }
}

#[cfg(test)]
mod tests;
