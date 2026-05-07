use crate::{
    EvmConfig, EvmTypes,
    interpreter::{
        instructions::table::unknown_instruction,
        private::{Instruction, InstructionImplFn},
    },
};
use core::fmt;

/// Type-specific version tables.
///
/// Stores the static gas table and instruction implementations for a concrete `EvmTypes` family.
/// These tables are compile-time inputs used to build the final interpreter dispatch table.
pub struct VersionTables<T: EvmTypes> {
    /// Static opcode gas table.
    static_gas_table: StaticGasTable,
    /// Instruction implementations.
    instruction_impls: InstructionImplTable<T>,
}

pub(crate) struct InstructionInfo<T: EvmTypes> {
    pub(crate) instr: InstructionImplFn<T>,
    pub(crate) dynamic_gas: bool,
    pub(crate) is_unknown: bool,
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
    pub(super) const fn empty() -> Self {
        Self {
            static_gas_table: StaticGasTable::empty(),
            instruction_impls: InstructionImplTable::empty(),
        }
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

    /// Returns instruction metadata for `opcode`.
    #[inline]
    pub(crate) const fn instruction(&self, opcode: u8) -> InstructionInfo<T> {
        self.instruction_impls.get(opcode)
    }

    /// Sets the static gas cost and instruction for `opcode`.
    ///
    /// An `I: Instruction` is implemented using the [`#[instruction]`](evm2_macros::instruction)
    /// proc macro.
    #[inline]
    pub const fn set_instruction<I: Instruction<T>>(&mut self, opcode: u8, gas: u16) {
        self.set_static_gas(opcode, gas);
        self.instruction_impls.set(opcode, I::execute, I::DYNAMIC_GAS);
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

struct InstructionImplTable<T: EvmTypes> {
    instrs: [Option<InstructionImplFn<T>>; 256],
    dynamic_gas: [bool; 256],
}

impl<T: EvmTypes> InstructionImplTable<T> {
    /// Creates an empty instruction implementation table.
    #[inline]
    const fn empty() -> Self {
        Self { instrs: [None; 256], dynamic_gas: [false; 256] }
    }

    /// Returns the instruction implementation for `opcode`.
    #[inline]
    const fn get(&self, opcode: u8) -> InstructionInfo<T> {
        match self.instrs[opcode as usize] {
            Some(instr) => InstructionInfo {
                instr,
                dynamic_gas: self.dynamic_gas[opcode as usize],
                is_unknown: false,
            },
            None => InstructionInfo {
                instr: unknown_instruction::<T>,
                dynamic_gas: true,
                is_unknown: true,
            },
        }
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline]
    const fn set(&mut self, opcode: u8, instr: InstructionImplFn<T>, dynamic_gas: bool) {
        self.instrs[opcode as usize] = Some(instr);
        self.dynamic_gas[opcode as usize] = dynamic_gas;
    }
}
