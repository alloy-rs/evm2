use crate::{
    EvmConfig, EvmTypes, Version,
    interpreter::{InstructionImplFn, instructions::table::unknown_instruction},
};
use core::fmt;

/// Type-specific version tables.
///
/// Stores the static gas table and instruction implementations for a concrete `EvmTypes` family.
/// These tables are compile-time inputs used to build the final interpreter dispatch table.
pub struct VersionTables<T: EvmTypes> {
    /// Active EVM version.
    version: Version,
    /// Static opcode gas table.
    static_gas_table: StaticGasTable,
    /// Instruction implementations.
    instruction_impls: InstructionImplTable<T>,
}

impl<T: EvmTypes> fmt::Debug for VersionTables<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VersionTables").finish_non_exhaustive()
    }
}

impl<T: EvmTypes> VersionTables<T> {
    /// Returns the base type-specific version tables for `C`.
    pub const fn base<C: EvmConfig<T>>() -> Self {
        super::base_version_tables::<T, C>()
    }

    /// Creates empty type-specific version tables.
    #[inline]
    pub(super) const fn empty(version: Version) -> Self {
        Self {
            version,
            static_gas_table: StaticGasTable::empty(),
            instruction_impls: InstructionImplTable::empty(),
        }
    }

    /// Returns the active EVM version.
    #[inline]
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// Returns the static gas cost for `opcode`.
    #[inline]
    pub const fn static_gas(&self, opcode: u8) -> u16 {
        self.static_gas_table.get(opcode)
    }

    /// Sets the static gas cost for `opcode`.
    #[inline]
    pub const fn set_static_gas(&mut self, opcode: u8, cost: u16) {
        self.static_gas_table.set(opcode, cost);
    }

    /// Returns `true` if `opcode` has a set instruction implementation.
    #[inline]
    pub const fn contains_instruction(&self, opcode: u8) -> bool {
        self.instruction_impls.contains(opcode)
    }

    /// Returns the instruction implementation for `opcode`.
    #[inline]
    pub const fn instruction(&self, opcode: u8) -> Option<InstructionImplFn<T>> {
        self.instruction_impls.get(opcode)
    }

    /// Returns the instruction implementation for `opcode`, or unknown if it is not set.
    #[inline]
    pub const fn instruction_or_unknown(&self, opcode: u8) -> InstructionImplFn<T> {
        self.instruction_impls.get_or_default(opcode)
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline]
    pub const fn set_instruction(&mut self, opcode: u8, instr: Option<InstructionImplFn<T>>) {
        self.instruction_impls.set(opcode, instr);
    }

    /// Sets the static gas cost and instruction implementation for `opcode`.
    #[inline]
    pub const fn set_opcode(&mut self, opcode: u8, gas: u16, instr: InstructionImplFn<T>) {
        self.set_static_gas(opcode, gas);
        self.set_instruction(opcode, Some(instr));
    }
}

struct StaticGasTable {
    table: [u16; 256],
    _align: [usize; 0],
}

impl StaticGasTable {
    /// Creates an empty gas table.
    #[inline]
    const fn empty() -> Self {
        Self { table: [0; 256], _align: [] }
    }

    /// Returns the gas cost for `opcode`.
    #[inline]
    const fn get(&self, opcode: u8) -> u16 {
        self.table[opcode as usize]
    }

    /// Sets the gas cost for `opcode`.
    #[inline]
    const fn set(&mut self, opcode: u8, cost: u16) {
        self.table[opcode as usize] = cost;
    }
}

struct InstructionImplTable<T: EvmTypes>([Option<InstructionImplFn<T>>; 256]);

impl<T: EvmTypes> InstructionImplTable<T> {
    /// Creates an empty instruction implementation table.
    #[inline]
    const fn empty() -> Self {
        Self([None; 256])
    }

    /// Returns `true` if `opcode` has a set instruction implementation.
    #[inline]
    const fn contains(&self, opcode: u8) -> bool {
        self.get(opcode).is_some()
    }

    /// Returns the instruction implementation for `opcode`.
    #[inline]
    const fn get(&self, opcode: u8) -> Option<InstructionImplFn<T>> {
        self.0[opcode as usize]
    }

    /// Returns the instruction implementation for `opcode`, or unknown if it is not set.
    #[inline]
    const fn get_or_default(&self, opcode: u8) -> InstructionImplFn<T> {
        match self.get(opcode) {
            Some(instr) => instr,
            None => unknown_instruction::<T>,
        }
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline]
    const fn set(&mut self, opcode: u8, instr: Option<InstructionImplFn<T>>) {
        self.0[opcode as usize] = instr;
    }
}
