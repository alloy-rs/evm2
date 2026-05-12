use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas},
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

dispatch_tables!();

pub(crate) type LoopState = ();

#[inline(always)]
pub(crate) const fn loop_state(_gas: &Gas) -> LoopState {}

#[inline(always)]
pub(crate) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    _loop_state: &mut LoopState,
) -> (Pc, usize) {
    let mut next_stack_len = stack.len;
    let next_pc = instr(pc, stack, state, &mut next_stack_len);
    (next_pc, next_stack_len)
}

#[inline(always)]
pub(crate) const fn finish_loop(_gas: &mut Gas, _loop_state: LoopState) {}

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
        dispatch_mono::<T, C, M>(pc, stack, state, next_stack_len, OP)
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    next_stack_len: &mut usize,
    op: u8,
) -> Pc {
    let instr = C::VERSION_TABLES.instruction(op).instr;
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match pre_step::<T, C>(state.gas_mut(), op) {
        Ok(()) => {
            r = instr(&mut pc, stack.as_mut(), state);
            if !M::INSPECT || r.is_ok() {
                super::inc_pc(&mut pc, op);
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
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(gas: &mut Gas, op: u8) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}
