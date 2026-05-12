use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Stack},
};

/// Unpacked instruction return value.
type InstrFnRet = (*const u8, usize);

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
    let (next_pc, next_stack_len) = instr(pc, stack, state);
    (Pc::new(next_pc), next_stack_len)
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
    ) -> InstrFnRet {
        let _ = DYNAMIC_GAS;
        let (pc, stack_len) = super::dispatch_mono::<T, C, M, OP>(
            pc,
            stack,
            state,
            C::VERSION_TABLES.instruction(OP).instr,
        );
        (pc.as_ptr(), stack_len)
    }

    pub(in crate::interpreter::dispatch) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        let (pc, stack_len) = super::dispatch_mono::<T, C, M, { super::UNKNOWN_OP }>(
            pc,
            stack,
            state,
            super::unknown_instruction,
        );
        (pc.as_ptr(), stack_len)
    }
}
