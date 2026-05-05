use crate::{
    EvmTypes,
    interpreter::{InstructionImplFn, instructions::table::unknown_instruction},
};
use core::ops::{Index, IndexMut};

/// Instruction implementation table.
#[derive(Clone, Copy)]
pub struct InstructionImplTable<T: EvmTypes>([Option<InstructionImplFn<T>>; 256]);

impl<T: EvmTypes> core::fmt::Debug for InstructionImplTable<T> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionImplTable").finish_non_exhaustive()
    }
}

impl<T: EvmTypes> Index<u8> for InstructionImplTable<T> {
    type Output = Option<InstructionImplFn<T>>;

    #[inline]
    fn index(&self, index: u8) -> &Self::Output {
        &self.0[index as usize]
    }
}

impl<T: EvmTypes> IndexMut<u8> for InstructionImplTable<T> {
    #[inline]
    fn index_mut(&mut self, index: u8) -> &mut Self::Output {
        &mut self.0[index as usize]
    }
}

impl<T: EvmTypes> InstructionImplTable<T> {
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
    pub const fn get(&self, opcode: u8) -> Option<InstructionImplFn<T>> {
        self.0[opcode as usize]
    }

    /// Returns the instruction implementation for `opcode`, or unknown if it is not set.
    #[inline]
    pub const fn get_or_default(&self, opcode: u8) -> InstructionImplFn<T> {
        match self.get(opcode) {
            Some(instr) => instr,
            None => unknown_instruction::<T>,
        }
    }

    /// Returns the mutable instruction implementation slot for `opcode`.
    #[inline]
    pub const fn get_mut(&mut self, opcode: u8) -> &mut Option<InstructionImplFn<T>> {
        &mut self.0[opcode as usize]
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline]
    pub const fn set(&mut self, opcode: u8, instr: Option<InstructionImplFn<T>>) {
        self.0[opcode as usize] = instr;
    }
}
