use super::{InspectMode, UNKNOWN_OP, inc_pc, run_state, unknown_instruction};
#[cfg(dispatch_packed)]
use crate::interpreter::gas::RemainingGas;
#[cfg(not(dispatch_packed))]
use crate::{
    EvmConfig,
    interpreter::{InterpreterState, Pc, Result, Stack, private::InstructionImplFn},
};
use crate::{
    EvmTypes,
    interpreter::{InstrStop, Interpreter, gas::Gas},
};
use core::hint::cold_path;

cfg_if::cfg_if! {
    if #[cfg(dispatch_packed)] {
        mod packed;
        use packed as imp;
    } else if #[cfg(dispatch_single_return)] {
        mod single_return;
        use single_return as imp;
    } else {
        mod unpacked;
        use unpacked as imp;
    }
}

pub(super) use imp::{RawInstrFn, dispatch, unknown_dispatch};

/// Table instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

#[cfg(dispatch_packed)]
type LoopState = RemainingGas;

#[cfg(not(dispatch_packed))]
type LoopState = ();

#[inline(always)]
#[cfg(dispatch_packed)]
const fn loop_state(gas: &Gas) -> LoopState {
    RemainingGas::new(gas.remaining())
}

#[inline(always)]
#[cfg(not(dispatch_packed))]
const fn loop_state(_gas: &Gas) -> LoopState {}

#[inline(always)]
#[cfg(dispatch_packed)]
const fn finish_loop(gas: &mut Gas, remaining_gas: LoopState) {
    gas.set_remaining(remaining_gas.get());
}

#[inline(always)]
#[cfg(not(dispatch_packed))]
const fn finish_loop(_gas: &mut Gas, _loop_state: LoopState) {}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
#[cfg(not(dispatch_packed))]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>, const OP: u8>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    instr: InstructionImplFn<T>,
) -> (Pc, usize) {
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match pre_step::<T, C, OP>(state.gas_mut()) {
        Ok(()) => {
            r = instr(&mut pc, stack.as_mut(), state);
            if !M::INSPECT || r.is_ok() {
                inc_pc(&mut pc, OP);
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
        return (Pc::new(core::ptr::null()), stack.len);
    }
    (pc, stack.len)
}

#[inline(always)]
#[cfg(not(dispatch_packed))]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>, const OP: u8>(gas: &mut Gas) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(OP) as _)
}

pub(in crate::interpreter) fn run<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let (state, mut pc, mut stack) = run_state(interpreter);
    let mut loop_state = loop_state(state.gas_mut());
    loop {
        let op = pc.op();
        let instr = instructions[op as usize];
        let (next_pc, next_stack_len) =
            imp::dispatch_loop_call(instr, pc, stack.reborrow(), state, &mut loop_state);
        pc = next_pc;
        stack.len = next_stack_len;

        if pc.as_ptr().is_null() {
            cold_path();
            state.set_pc_stack_len(pc.as_ptr(), stack.len);
            finish_loop(state.gas_mut(), loop_state);
            return state.result().unwrap_err();
        }
    }
}
