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

impl<C: EvmConfig> Index<u8> for InstructionImplTable<C> {
    type Output = Option<&'static dyn Instruction<C>>;

    #[inline]
    fn index(&self, index: u8) -> &Self::Output {
        &self.0[index as usize]
    }
}

impl<C: EvmConfig> IndexMut<u8> for InstructionImplTable<C> {
    #[inline]
    fn index_mut(&mut self, index: u8) -> &mut Self::Output {
        &mut self.0[index as usize]
    }
}

impl<C: EvmConfig> InstructionImplTable<C> {
    /// Creates an empty instruction implementation table.
    #[inline]
    pub(super) const fn empty() -> Self {
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
