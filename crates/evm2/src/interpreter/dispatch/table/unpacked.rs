use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas, private::InstructionImplFn},
};
use core::hint::cold_path;

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
        dispatch_mono::<T, C, M, OP>(
            pc,
            stack,
            state,
            C::VERSION_TABLES.instruction(OP).instr,
        )
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
        dispatch_mono::<T, C, M, { super::UNKNOWN_OP }>(
            pc,
            stack,
            state,
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
    instr: InstructionImplFn<T>,
) -> InstrFnRet {
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
    if r.is_err() {
        cold_path();
        if !M::INSPECT {
            state.set_result(r);
        }
        return (core::ptr::null(), stack.len);
    }
    (pc.as_ptr(), stack.len)
}

#[inline(always)]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>, const OP: u8>(gas: &mut Gas) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(OP) as _)
}
