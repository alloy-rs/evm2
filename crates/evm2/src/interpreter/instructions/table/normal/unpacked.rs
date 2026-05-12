use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas},
};
use core::hint::cold_path;

/// Unpacked normal instruction return value.
type InstrFnRet = (*const u8, usize);

/// Unpacked normal instruction function pointer.
pub(super) type RawInstrFn<T> =
    extern_table!(fn(pc: Pc, stack: Stack<'_>, state: &mut InterpreterState<'_, T>) -> InstrFnRet);

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
                super::super::inc_pc(&mut pc, op);
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
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(gas: &mut Gas, op: u8) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}
