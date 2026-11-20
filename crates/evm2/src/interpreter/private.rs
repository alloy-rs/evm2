use super::{Gas, InterpreterState, Pc, Result, StackMut, Word};
use crate::EvmTypesHost;
use bitflags::bitflags;

/// Function signature of an `#[instruction]`.
pub(crate) type InstructionImplFn<T> =
    fn(pc: &mut Pc, stack: StackMut<'_>, state: &mut InterpreterState<'_, '_, T>) -> Result;

bitflags! {
    /// Properties of an [`Instruction`] that the dispatch acts on.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct InstructionFlags: u64 {
        /// The instruction needs mutable gas state.
        const DYNAMIC_GAS = 1 << 0;
        /// The instruction may run inspector hooks or host code that read the interpreter, so the
        /// dispatch stores the latest pc, stack length and gas into it first. Implies
        /// [`Self::DYNAMIC_GAS`].
        const HOOKED = 1 << 1;
        /// The instruction sets the program counter itself, so the dispatch does not advance it.
        const SETS_PC = 1 << 2;
    }
}

/// EVM instruction implementation.
pub trait Instruction<T: EvmTypesHost = crate::BaseEvmTypes> {
    /// The properties of this instruction.
    const FLAGS: InstructionFlags = InstructionFlags::DYNAMIC_GAS;

    /// The instruction's effect on its stack slots, without the stack, for instructions with a
    /// fixed number of stack inputs and outputs. `#[instruction]` sets it unless
    /// `no_stack_preamble`.
    const LOGIC: Option<InstructionLogic<T>> = None;

    /// Executes this instruction.
    fn execute(pc: &mut Pc, stack: StackMut<'_>, state: &mut InterpreterState<'_, '_, T>)
    -> Result;
}

/// An instruction's effect on the stack slots it reads and writes, without the stack.
///
/// [`Self::run`] gets the `max(inputs, outputs)` slots the instruction uses, deepest first, in the
/// stack's own layout: the inputs fill the first `inputs` slots with the top of the stack last,
/// and the instruction leaves its outputs in the first `outputs` slots the same way. It does
/// everything else the instruction does, such as charging dynamic gas or moving `pc`, but checks
/// no stack bounds and charges no static gas, which the dispatch does.
pub struct InstructionLogic<T: EvmTypesHost> {
    /// Stack words the instruction reads.
    pub inputs: u8,
    /// Stack words the instruction writes.
    pub outputs: u8,
    /// Runs the instruction on its slots.
    pub run: InstructionLogicFn<T>,
}

/// Function signature of [`InstructionLogic::run`].
pub type InstructionLogicFn<T> =
    fn(pc: &mut Pc, state: &mut InterpreterState<'_, '_, T>, slots: &mut [Word]) -> Result;

impl<T: EvmTypesHost> Clone for InstructionLogic<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: EvmTypesHost> Copy for InstructionLogic<T> {}

impl<T: EvmTypesHost> core::fmt::Debug for InstructionLogic<T> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionLogic")
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .finish_non_exhaustive()
    }
}

/// Instruction execution context.
pub struct InstructionCx<'a, 'state, 'host, T: EvmTypesHost> {
    /// Program counter state.
    pub pc: &'a mut Pc,
    /// Interpreter state.
    pub state: &'a mut InterpreterState<'state, 'host, T>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Instruction execution context with mutable gas state.
pub struct GasInstructionCx<'a, 'state, 'host, T: EvmTypesHost> {
    /// Program counter state.
    pub pc: &'a mut Pc,
    /// Gas state.
    pub gas: &'a mut Gas,
    /// Interpreter state.
    pub state: &'a mut InterpreterState<'state, 'host, T>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypesHost> core::fmt::Debug for InstructionCx<'_, '_, '_, T> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstructionCx").finish_non_exhaustive()
    }
}

impl<T: EvmTypesHost> core::fmt::Debug for GasInstructionCx<'_, '_, '_, T> {
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
pub unsafe fn split_gas_state<'a, 'state, 'host, T: EvmTypesHost>(
    state: *mut InterpreterState<'state, 'host, T>,
) -> (&'a mut Gas, &'a mut InterpreterState<'state, 'host, T>) {
    // The overlapping `gas` and `state` references are intentional and have already been reviewed.
    // This overlap alone is not considered an actionable UB finding, including Miri aliasing
    // diagnostics. The safety requirement above still applies.
    // SAFETY: The caller must ensure the returned `gas` reference is not used through `state`.
    unsafe { (&mut *InterpreterState::gas_from_state_ptr(state), &mut *state) }
}
