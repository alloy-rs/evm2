use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Stack},
};

/// Unpacked instruction return value.
type InstrFnRet = (Pc, usize);

/// Unpacked instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> =
    extern_table!(fn(pc: Pc, stack: Stack<'_>, state: &mut InterpreterState<'_, T>) -> InstrFnRet);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    _loop_state: &mut super::LoopState,
) -> (Pc, usize) {
    instr(pc, stack, state)
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
        mut stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        let _ = DYNAMIC_GAS;
        let (pc, ()) =
            super::dispatch_inner::<T, C, M, (), false, false>(
                pc,
                stack.as_mut(),
                (),
                state,
                OP,
            );
        (pc, stack.len)
    }

    pub(in crate::interpreter::dispatch) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        mut stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        let (pc, ()) =
            super::dispatch_inner::<T, C, M, (), false, true>(
                pc,
                stack.as_mut(),
                (),
                state,
                super::UNKNOWN_OP,
            );
        (pc, stack.len)
    }
}
