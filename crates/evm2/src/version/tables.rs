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
    /// Per-opcode revision for dispatch-table rebuild decisions.
    revisions: [u8; 256],
}

pub(crate) struct InstructionInfo<T: EvmTypes> {
    pub(crate) instr: InstructionImplFn<T>,
    #[allow(dead_code)]
    pub(crate) dynamic_gas: bool,
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
            revisions: [0; 256],
        }
    }

    /// Returns the static gas cost for `opcode`.
    #[inline(always)]
    pub const fn static_gas(&self, opcode: u8) -> u16 {
        self.static_gas_table.get(opcode)
    }

    /// Returns the dispatch-table revision for `opcode`.
    #[inline(always)]
    pub(crate) const fn revision(&self, opcode: u8) -> u8 {
        self.revisions[opcode as usize]
    }

    /// Sets the static gas cost for `opcode`.
    #[inline(always)]
    pub const fn set_static_gas(&mut self, opcode: u8, cost: u16) {
        if self.static_gas_table.set(opcode, cost) {
            self.bump_revision(opcode);
        }
    }

    /// Returns instruction metadata for `opcode`.
    #[inline(always)]
    pub(crate) const fn instruction(&self, opcode: u8) -> InstructionInfo<T> {
        self.instruction_impls.get(opcode)
    }

    /// Returns whether `opcode` has no instruction implementation in this version.
    #[inline(always)]
    pub(crate) const fn is_unknown_opcode(&self, opcode: u8) -> bool {
        self.instruction_impls.is_unknown(opcode)
    }

    /// Sets the static gas cost and instruction for `opcode`.
    ///
    /// An `I: Instruction` is implemented using the [`#[instruction]`](evm2_macros::instruction)
    /// proc macro.
    #[inline(always)]
    pub const fn set_instruction<I: Instruction<T>>(&mut self, opcode: u8, gas: u16) {
        self.set_static_gas(opcode, gas);
        if self.instruction_impls.set(opcode, I::execute, I::DYNAMIC_GAS) {
            self.bump_revision(opcode);
        }
    }

    #[inline(always)]
    const fn bump_revision(&mut self, opcode: u8) {
        self.revisions[opcode as usize] += 1;
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
    #[inline(always)]
    const fn get(&self, opcode: u8) -> u16 {
        self.table[opcode as usize]
    }

    /// Sets the gas cost for `opcode`.
    #[inline(always)]
    const fn set(&mut self, opcode: u8, cost: u16) -> bool {
        let index = opcode as usize;
        if self.table[index] != cost {
            self.table[index] = cost;
            return true;
        }
        false
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
    #[inline(always)]
    const fn get(&self, opcode: u8) -> InstructionInfo<T> {
        match self.instrs[opcode as usize] {
            Some(instr) => {
                InstructionInfo { instr, dynamic_gas: self.dynamic_gas[opcode as usize] }
            }
            None => InstructionInfo { instr: unknown_instruction::<T>, dynamic_gas: true },
        }
    }

    /// Returns whether `opcode` has no instruction implementation.
    #[inline(always)]
    const fn is_unknown(&self, opcode: u8) -> bool {
        self.instrs[opcode as usize].is_none()
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline(always)]
    const fn set(&mut self, opcode: u8, instr: InstructionImplFn<T>, dynamic_gas: bool) -> bool {
        let index = opcode as usize;
        self.instrs[index] = Some(instr);
        self.dynamic_gas[index] = dynamic_gas;
        true
    }
}
