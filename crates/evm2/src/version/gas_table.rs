use crate::{BaseEvmTypes, EvmVersion, interpreter::SpecId};
use core::ops::{Index, IndexMut};

/// Opcode gas table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GasTable(pub(crate) [u16; 256]);

impl Index<usize> for GasTable {
    type Output = u16;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl IndexMut<usize> for GasTable {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl GasTable {
    /// Creates a gas table for `spec`.
    #[inline]
    pub const fn new(spec: SpecId) -> Self {
        EvmVersion::<BaseEvmTypes>::new_base(spec).gas_table
    }

    /// Returns the gas cost for `opcode`.
    #[inline]
    pub const fn get(&self, opcode: u8) -> u16 {
        self.0[opcode as usize]
    }

    /// Returns the mutable gas cost slot for `opcode`.
    #[inline]
    pub const fn get_mut(&mut self, opcode: u8) -> &mut u16 {
        &mut self.0[opcode as usize]
    }

    /// Sets the gas cost for `opcode`.
    #[inline]
    pub const fn set(&mut self, opcode: u8, cost: u16) {
        self.0[opcode as usize] = cost;
    }
}
