use super::{Gas, InterpreterState, Pc, Result, StackMut, Word};
use crate::EvmTypes;

/// Function signature of an `#[instruction]`.
pub(crate) type InstructionImplFn<T> =
    fn(pc: &mut Pc, stack: StackMut<'_>, state: &mut InterpreterState<'_, T>) -> Result;

/// EVM instruction implementation.
pub trait Instruction<T: EvmTypes = crate::BaseEvmTypes> {
    /// Whether this instruction needs mutable gas state.
    const DYNAMIC_GAS: bool = true;

    /// Executes this instruction.
    fn execute(pc: &mut Pc, stack: StackMut<'_>, state: &mut InterpreterState<'_, T>) -> Result;
}

/// Instruction execution context.
pub struct InstructionCx<'a, 'state, T: EvmTypes> {
    /// Program counter state.
    pub pc: &'a mut Pc,
    /// Interpreter state.
    pub state: &'a mut InterpreterState<'state, T>,
}

/// Instruction execution context with mutable gas state.
pub struct GasInstructionCx<'a, 'state, T: EvmTypes> {
    /// Program counter state.
    pub pc: &'a mut Pc,
    /// Gas state.
    pub gas: &'a mut Gas,
    /// Interpreter state.
    pub state: &'a mut InterpreterState<'state, T>,
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

/// Splits a mutable instruction state into separate gas and state references.
///
/// # Safety
///
/// The returned `gas` reference must not be accessed through the returned `state` reference while
/// both references are live.
#[inline]
pub unsafe fn split_gas_state<'a, 'state, T: EvmTypes>(
    state: *mut InterpreterState<'state, T>,
) -> (&'a mut Gas, &'a mut InterpreterState<'state, T>) {
    // SAFETY: The caller must ensure the returned `gas` reference is not used through `state`.
    unsafe { (&mut *InterpreterState::gas_from_state_ptr(state), &mut *state) }
}
