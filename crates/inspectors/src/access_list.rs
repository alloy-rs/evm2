//! Access list inspector.

use alloc::collections::BTreeSet;
use alloy_primitives::{
    Address, B256,
    map::{HashMap, HashSet},
};
use alloy_rpc_types_eth::{AccessList, AccessListItem};
use evm2::{
    EvmTypes, Inspector,
    bytecode::opcode,
    interpreter::{Interpreter, Message, MessageResult, Word},
};

/// An [Inspector] that collects touched accounts and storage slots.
#[derive(Debug, Default)]
pub struct AccessListInspector {
    /// All addresses that should be excluded from the final accesslist.
    excluded: HashSet<Address>,
    /// All addresses and touched slots.
    touched_slots: HashMap<Address, BTreeSet<B256>>,
}

impl From<AccessList> for AccessListInspector {
    fn from(access_list: AccessList) -> Self {
        Self::new(access_list)
    }
}

impl AccessListInspector {
    /// Creates a new inspector instance.
    pub fn new(access_list: AccessList) -> Self {
        Self {
            excluded: Default::default(),
            touched_slots: access_list
                .0
                .into_iter()
                .map(|v| (v.address, v.storage_keys.into_iter().collect()))
                .collect(),
        }
    }

    /// Returns the excluded addresses.
    pub const fn excluded(&self) -> &HashSet<Address> {
        &self.excluded
    }

    /// Returns a reference to the map of addresses and their corresponding touched storage slots.
    pub const fn touched_slots(&self) -> &HashMap<Address, BTreeSet<B256>> {
        &self.touched_slots
    }

    /// Consumes the inspector and returns the map of addresses and their corresponding touched
    /// storage slots.
    pub fn into_touched_slots(self) -> HashMap<Address, BTreeSet<B256>> {
        self.touched_slots
    }

    /// Returns list of addresses and storage keys used by the transaction.
    pub fn into_access_list(self) -> AccessList {
        let items = self.touched_slots.into_iter().map(|(address, slots)| AccessListItem {
            address,
            storage_keys: slots.into_iter().collect(),
        });
        AccessList(items.collect())
    }

    /// Returns list of addresses and storage keys used by the transaction.
    pub fn access_list(&self) -> AccessList {
        let items = self.touched_slots.iter().map(|(address, slots)| AccessListItem {
            address: *address,
            storage_keys: slots.iter().copied().collect(),
        });
        AccessList(items.collect())
    }

    fn collect_excluded_addresses<T: EvmTypes>(&mut self, message: &Message<T>) {
        self.excluded.insert(message.caller);
        self.excluded.insert(message.destination);
    }
}

impl<T: EvmTypes> Inspector<T> for AccessListInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        match interp.opcode() {
            opcode::SLOAD | opcode::SSTORE => {
                if let Some(slot) = stack_peek(interp, 0) {
                    let cur_contract = interp.message().destination;
                    self.touched_slots
                        .entry(cur_contract)
                        .or_default()
                        .insert(B256::from(slot.to_be_bytes()));
                }
            }
            opcode::EXTCODECOPY
            | opcode::EXTCODEHASH
            | opcode::EXTCODESIZE
            | opcode::BALANCE
            | opcode::SELFDESTRUCT => {
                if let Some(slot) = stack_peek(interp, 0) {
                    let addr = Address::from_word(B256::from(slot.to_be_bytes()));
                    if !self.excluded.contains(&addr) {
                        self.touched_slots.entry(addr).or_default();
                    }
                }
            }
            opcode::DELEGATECALL | opcode::CALL | opcode::STATICCALL | opcode::CALLCODE => {
                if let Some(slot) = stack_peek(interp, 1) {
                    let addr = Address::from_word(B256::from(slot.to_be_bytes()));
                    if !self.excluded.contains(&addr) {
                        self.touched_slots.entry(addr).or_default();
                    }
                }
            }
            _ => (),
        }
    }

    fn call(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        if message.depth == 1 {
            self.collect_excluded_addresses(message);
        }
        None
    }

    fn create(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        if message.depth == 1 {
            self.collect_excluded_addresses(message);
        }
        None
    }
}

#[inline]
fn stack_peek<T: EvmTypes>(interp: &Interpreter<'_, T>, index_from_top: usize) -> Option<Word> {
    let stack = interp.stack();
    stack.get(stack.len().checked_sub(index_from_top + 1)?).copied()
}
