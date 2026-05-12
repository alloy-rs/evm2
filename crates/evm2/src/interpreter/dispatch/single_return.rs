use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas, private::InstructionImplFn},
};
use core::hint::cold_path;

/// Single-return instruction function pointer.
pub(super) type RawInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
        next_stack_len: &mut usize,
    ) -> Pc
);

/// Single-return instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

#[inline(always)]
pub(super) const fn loop_state(_gas: &Gas) {}

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    _loop_state: &mut (),
) -> (Pc, usize) {
    let mut next_stack_len = stack.len;
    let next_pc = instr(pc, stack, state, &mut next_stack_len);
    (next_pc, next_stack_len)
}

#[inline(always)]
pub(super) const fn finish_loop(_gas: &mut Gas, _loop_state: ()) {}

extern_table! {
    pub(super) fn dispatch<
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
        dispatch_mono::<T, C, M, OP>(
            pc,
            stack,
            state,
            next_stack_len,
            C::VERSION_TABLES.instruction(OP).instr,
        )
    }

    pub(super) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
        next_stack_len: &mut usize,
    ) -> Pc {
        dispatch_mono::<T, C, M, { super::UNKNOWN_OP }>(
            pc,
            stack,
            state,
            next_stack_len,
            super::unknown_instruction,
        )
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>, const OP: u8>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    next_stack_len: &mut usize,
    instr: InstructionImplFn<T>,
) -> Pc {
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match pre_step::<T, C, OP>(state.gas_mut()) {
        Ok(()) => {
            r = instr(&mut pc, stack.as_mut(), state);
            if !M::INSPECT || r.is_ok() {
                super::inc_pc(&mut pc, OP);
            }
        }
        Err(e) => r = Err(e),
    }
    if M::INSPECT {
        state.set_result(r);
        M::step_end(state, pc, stack.len);
    }
    *next_stack_len = stack.len;
    if r.is_err() {
        cold_path();
        if !M::INSPECT {
            state.set_result(r);
        }
        return Pc::new(core::ptr::null());
    }
    pc
}

#[inline(always)]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>, const OP: u8>(gas: &mut Gas) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(OP) as _)
}
