use core::ops::{Index, IndexMut};

/// Opcode gas table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaticGasTable {
    table: [u16; 256],
    _align: [usize; 0],
}

impl Index<u8> for StaticGasTable {
    type Output = u16;

    #[inline]
    fn index(&self, index: u8) -> &Self::Output {
        &self.table[index as usize]
    }
}

impl IndexMut<u8> for StaticGasTable {
    #[inline]
    fn index_mut(&mut self, index: u8) -> &mut Self::Output {
        &mut self.table[index as usize]
    }
}

impl StaticGasTable {
    /// Creates an empty gas table.
    #[inline]
    pub(super) const fn empty() -> Self {
        Self { table: [0; 256], _align: [] }
    }

    /// Returns the gas cost for `opcode`.
    #[inline]
    pub const fn get(&self, opcode: u8) -> u16 {
        self.table[opcode as usize]
    }

    /// Returns the mutable gas cost slot for `opcode`.
    #[inline]
    pub const fn get_mut(&mut self, opcode: u8) -> &mut u16 {
        &mut self.table[opcode as usize]
    }

    /// Sets the gas cost for `opcode`.
    #[inline]
    pub const fn set(&mut self, opcode: u8, cost: u16) {
        self.table[opcode as usize] = cost;
    }
}
