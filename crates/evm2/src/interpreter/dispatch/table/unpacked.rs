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
        const OP: u8,
    >(
        pc: Pc,
        mut stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        let opcode = OP;
        let (pc, ()) = super::dispatch_inner::<T, C, ()>(pc, stack.as_mut(), (), state, opcode);
        (pc, stack.len)
    }
}
