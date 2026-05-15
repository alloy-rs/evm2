use alloy_primitives::{Address, B256, map::HashMap};
use evm2::{EvmTypes, Inspector, bytecode::opcode::op, interpreter::Interpreter};

/// An Inspector that tracks warm and cold storage slot accesses.
#[derive(Debug, Default, Clone)]
pub struct StorageInspector {
    /// Tracks storage slots and access counter.
    accessed_slots: HashMap<Address, HashMap<B256, u64>>,
}

impl StorageInspector {
    /// Creates a new storage inspector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of accessed slots that were only accessed once.
    pub fn unique_loads(&self) -> u64 {
        self.accessed_slots
            .values()
            .flat_map(|slots| slots.values())
            .filter(|&&count| count == 1)
            .count() as u64
    }

    /// Returns how often slots where accessed after the initial access.
    pub fn warm_loads(&self) -> u64 {
        self.accessed_slots
            .values()
            .flat_map(|slots| slots.values())
            .map(|&count| count.saturating_sub(1))
            .sum()
    }

    /// Returns the tracked slots per address.
    pub const fn accessed_slots(&self) -> &HashMap<Address, HashMap<B256, u64>> {
        &self.accessed_slots
    }

    /// Consumes the inspector and returns the map.
    pub fn into_accessed_slots(self) -> HashMap<Address, HashMap<B256, u64>> {
        self.accessed_slots
    }
}

impl<T: EvmTypes> Inspector<T> for StorageInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        if interp.opcode() == op::SLOAD
            && let Some([slot]) = interp.stack().peekn()
        {
            let address = interp.message().destination;
            let slot = B256::from(slot.to_be_bytes());

            let slot_access_count =
                self.accessed_slots.entry(address).or_default().entry(slot).or_default();

            *slot_access_count += 1;
        }
    }
}
