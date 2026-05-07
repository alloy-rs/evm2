use super::{Gas, Pc, Result, StackMut, State, Word};
use crate::EvmTypes;

/// Instruction execution context.
pub struct InstructionCx<'a, 'state, T: EvmTypes> {
    /// Program counter state.
    pub pc: &'a mut Pc,
    /// Interpreter state.
    pub state: &'a mut State<'state, T>,
}

/// Instruction execution context with mutable gas state.
pub struct GasInstructionCx<'a, 'state, T: EvmTypes> {
    /// Program counter state.
    pub pc: &'a mut Pc,
    /// Gas state.
    pub gas: &'a mut Gas,
    /// Interpreter state.
    pub state: &'a mut State<'state, T>,
}

impl<T: EvmTypes> core::fmt::Debug for InstructionCx<'_, '_, T> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionCx").finish_non_exhaustive()
    }
}

impl<T: EvmTypes> core::fmt::Debug for GasInstructionCx<'_, '_, T> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GasInstructionCx").finish_non_exhaustive()
    }
}

#[inline(always)]
pub fn instr_stack_setup(
    stack: &mut StackMut<'_>,
    input: usize,
    output: usize,
) -> Result<*mut Word> {
    stack.instr_stack_setup(input, output)
}

#[inline]
pub fn gas<'a, T: EvmTypes>(state: &mut State<'a, T>) -> &'a mut Gas {
    state.gas()
}
