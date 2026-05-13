use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Stack, StackMut},
};

/// Single-return instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> =
    extern_table!(fn(pc: Pc, stack: StackMut<'_>, state: &mut InterpreterState<'_, T>) -> Pc);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    _loop_state: &mut super::LoopState,
) -> (Pc, usize) {
    let next_pc = instr(pc, stack.as_mut(), state);
    (next_pc, stack.len)
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
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> Pc {
        let _ = DYNAMIC_GAS;
        let (pc, ()) =
            super::dispatch_inner::<T, C, M, (), false, false>(pc, stack, (), state, OP);
        pc
    }

    pub(in crate::interpreter::dispatch) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> Pc {
        let (pc, ()) =
            super::dispatch_inner::<T, C, M, (), false, true>(
                pc,
                stack,
                (),
                state,
                super::UNKNOWN_OP,
            );
        pc
    }
}
