use alloc::collections::BTreeSet;
use alloy_primitives::{
    Address, B256,
    map::{HashMap, HashSet},
};
use alloy_rpc_types_eth::{AccessList, AccessListItem};
use evm2::{
    Evm, EvmTypes, Inspector,
    bytecode::opcode::op,
    interpreter::{Interpreter, Message, MessageResult},
};

/// An [Inspector] that collects touched accounts and storage slots.
///
/// This can be used to construct an [AccessList] for a transaction via `eth_createAccessList`
#[derive(Debug, Default)]
pub struct AccessListInspector {
    /// All addresses that should be excluded from the final accesslist
    excluded: HashSet<Address>,
    /// All addresses and touched slots
    touched_slots: HashMap<Address, BTreeSet<B256>>,
}

impl From<AccessList> for AccessListInspector {
    fn from(access_list: AccessList) -> Self {
        Self::new(access_list)
    }
}

impl AccessListInspector {
    /// Creates a new inspector instance
    ///
    /// The `access_list` is the provided access list from the call request
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

    /// Returns list of addresses and storage keys used by the transaction. It gives you the list of
    /// addresses and storage keys that were touched during execution.
    pub fn into_access_list(self) -> AccessList {
        let items = self.touched_slots.into_iter().map(|(address, slots)| AccessListItem {
            address,
            storage_keys: slots.into_iter().collect(),
        });
        AccessList(items.collect())
    }

    /// Returns list of addresses and storage keys used by the transaction. It gives you the list of
    /// addresses and storage keys that were touched during execution.
    pub fn access_list(&self) -> AccessList {
        let items = self.touched_slots.iter().map(|(address, slots)| AccessListItem {
            address: *address,
            storage_keys: slots.iter().copied().collect(),
        });
        AccessList(items.collect())
    }

    /// Collects addresses which should be excluded from the access list. Must be called before the
    /// top-level call.
    ///
    /// Those include caller, callee and precompiles.
    fn collect_excluded_addresses<T: EvmTypes<Host = Evm<T>>>(
        &mut self,
        message: &Message<T>,
        host: &Evm<T>,
    ) {
        self.excluded = [message.caller, message.destination]
            .into_iter()
            .chain(host.precompiles().warm_addresses())
            .chain(host.eip7702_authorities())
            .collect();
    }
}

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for AccessListInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        match interp.opcode() {
            op::SLOAD | op::SSTORE => {
                if let Some([slot]) = interp.stack().peekn() {
                    let cur_contract = interp.message().destination;
                    self.touched_slots
                        .entry(cur_contract)
                        .or_default()
                        .insert(B256::from(slot.to_be_bytes()));
                }
            }
            op::EXTCODECOPY
            | op::EXTCODEHASH
            | op::EXTCODESIZE
            | op::BALANCE
            | op::SELFDESTRUCT => {
                if let Some([slot]) = interp.stack().peekn() {
                    let addr = Address::from_word(B256::from(slot.to_be_bytes()));
                    if !self.excluded.contains(&addr) {
                        self.touched_slots.entry(addr).or_default();
                    }
                }
            }
            op::DELEGATECALL | op::CALL | op::STATICCALL | op::CALLCODE => {
                if let Some([_, slot]) = interp.stack().peekn() {
                    let addr = Address::from_word(B256::from(slot.to_be_bytes()));
                    if !self.excluded.contains(&addr) {
                        self.touched_slots.entry(addr).or_default();
                    }
                }
            }
            _ => (),
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        // At the top-level frame, fill the excluded addresses.
        if message.depth == 0 {
            self.collect_excluded_addresses(message, interp.host());
        }
        None
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        // At the top-level frame, fill the excluded addresses.
        if message.depth == 0 {
            self.collect_excluded_addresses(message, interp.host());
        }
        None
    }
}
