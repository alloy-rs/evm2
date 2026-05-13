use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Stack, StackMut, gas::Gas},
};

pub(super) type LoopState = ();

/// Single-return instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> =
    extern_table!(fn(pc: Pc, stack: StackMut<'_>, state: &mut InterpreterState<'_, T>) -> Pc);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    _loop_state: &mut LoopState,
) -> (Pc, usize) {
    let next_pc = instr(pc, stack.as_mut(), state);
    (next_pc, stack.len)
}

#[inline(always)]
pub(super) const fn loop_state(_gas: &Gas) -> LoopState {}

#[inline(always)]
pub(super) const fn finish_loop(_gas: &mut Gas, _loop_state: LoopState) {}

#[inline(always)]
pub(super) const fn sync_loop_state<T: EvmTypes>(
    _state: &mut InterpreterState<'_, T>,
    _loop_state: LoopState,
) {
}

extern_table! {
    pub(in crate::interpreter::dispatch) fn dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        const OP: u8,
    >(
        pc: Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> Pc {
        let (pc, ()) = super::dispatch_inner::<T, C, ()>(pc, stack, (), state, OP);
        pc
    }
}
