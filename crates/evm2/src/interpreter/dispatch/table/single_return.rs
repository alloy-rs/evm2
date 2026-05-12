use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Stack},
};

/// Single-return instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
        next_stack_len: &mut usize,
    ) -> Pc
);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    _loop_state: &mut super::LoopState,
) -> (Pc, usize) {
    let mut next_stack_len = stack.len;
    let next_pc = instr(pc, stack, state, &mut next_stack_len);
    (next_pc, next_stack_len)
}

extern_table! {
    pub(in crate::interpreter::dispatch) fn dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const OP: u8,
        const DYNAMIC_GAS: bool,
    >(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
        next_stack_len: &mut usize,
    ) -> Pc {
        let _ = DYNAMIC_GAS;
        let (pc, stack_len) = super::dispatch_mono::<T, C, M, false>(pc, stack, state, OP);
        *next_stack_len = stack_len;
        pc
    }

    pub(in crate::interpreter::dispatch) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
        next_stack_len: &mut usize,
    ) -> Pc {
        let (pc, stack_len) =
            super::dispatch_mono::<T, C, M, true>(pc, stack, state, super::UNKNOWN_OP);
        *next_stack_len = stack_len;
        pc
    }
}
