use crate::{
    EvmConfig,
    interpreter::{table::Instruction, unknown},
};
use core::ops::{Index, IndexMut};

/// Instruction implementation table.
#[derive(Clone, Copy)]
pub struct InstructionImplTable<C: EvmConfig>([Option<&'static dyn Instruction<C>>; 256]);

impl<C: EvmConfig> core::fmt::Debug for InstructionImplTable<C> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionImplTable").finish_non_exhaustive()
    }
}

impl<C: EvmConfig> Index<usize> for InstructionImplTable<C> {
    type Output = Option<&'static dyn Instruction<C>>;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<C: EvmConfig> IndexMut<usize> for InstructionImplTable<C> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl<C: EvmConfig> Default for InstructionImplTable<C> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<C: EvmConfig> InstructionImplTable<C> {
    /// Creates an instruction implementation table.
    #[inline]
    pub const fn new() -> Self {
        Self([None; 256])
    }

    /// Returns `true` if `opcode` has a set instruction implementation.
    #[inline]
    pub const fn contains(&self, opcode: u8) -> bool {
        self.get(opcode).is_some()
    }

    /// Returns the instruction implementation for `opcode`.
    #[inline]
    pub const fn get(&self, opcode: u8) -> Option<&'static dyn Instruction<C>> {
        self.0[opcode as usize]
    }

    /// Returns the instruction implementation for `opcode`, or unknown if it is not set.
    #[inline]
    pub const fn get_or_default(&self, opcode: u8) -> &'static dyn Instruction<C> {
        match self.get(opcode) {
            Some(instr) => instr,
            None => &unknown,
        }
    }

    /// Returns the mutable instruction implementation slot for `opcode`.
    #[inline]
    pub const fn get_mut(&mut self, opcode: u8) -> &mut Option<&'static dyn Instruction<C>> {
        &mut self.0[opcode as usize]
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline]
    pub const fn set(&mut self, opcode: u8, instr: Option<&'static dyn Instruction<C>>) {
        self.0[opcode as usize] = instr;
    }
}
