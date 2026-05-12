use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas},
};
use core::hint::cold_path;

/// Unpacked instruction return value.
type InstrFnRet = (*const u8, usize);

/// Unpacked instruction function pointer.
pub(super) type RawInstrFn<T> =
    extern_table!(fn(pc: Pc, stack: Stack<'_>, state: &mut InterpreterState<'_, T>) -> InstrFnRet);

/// Unpacked instruction dispatch table.
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
    let (next_pc, next_stack_len) = instr(pc, stack, state);
    (Pc::new(next_pc), next_stack_len)
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
    ) -> InstrFnRet {
        let _ = DYNAMIC_GAS;
        dispatch_mono::<T, C, M>(pc, stack, state, OP)
    }

    pub(super) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        dispatch_unknown::<T, C, M>(pc, stack, state)
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    op: u8,
) -> InstrFnRet {
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
    if r.is_err() {
        cold_path();
        if !M::INSPECT {
            state.set_result(r);
        }
        return (core::ptr::null(), stack.len);
    }
    (pc.as_ptr(), stack.len)
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_unknown<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
) -> InstrFnRet {
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match pre_step::<T, C>(state.gas_mut(), pc.op()) {
        Ok(()) => r = super::unknown_instruction(&mut pc, stack.as_mut(), state),
        Err(e) => r = Err(e),
    }
    if M::INSPECT {
        state.set_result(r);
        M::step_end(state, pc, stack.len);
    }
    cold_path();
    if !M::INSPECT {
        state.set_result(r);
    }
    (core::ptr::null(), stack.len)
}

#[inline(always)]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(gas: &mut Gas, op: u8) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}
