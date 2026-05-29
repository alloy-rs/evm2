use super::{DynInspector, InspectMode, inc_pc, run_state};
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InstrStop, Interpreter, InterpreterState, Pc, Result, Stack, StackMut},
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

pub(super) use imp::{RawInstrFn, dispatch};

/// Table instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

trait DispatchGas: Copy {
    fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        op: u8,
    ) -> Result;

    fn sync_before_inspect<T: EvmTypes>(&self, state: &mut InterpreterState<'_, T>);

    fn sync_before_exec<T: EvmTypes>(&self, state: &mut InterpreterState<'_, T>, dynamic_gas: bool);

    fn sync_after_exec<T: EvmTypes>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
    );
}

impl DispatchGas for () {
    #[inline(always)]
    fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        op: u8,
    ) -> Result {
        state.gas_mut().spend(C::OPCODE_CONFIG.static_gas(op) as _)
    }

    #[inline(always)]
    fn sync_before_inspect<T: EvmTypes>(&self, _state: &mut InterpreterState<'_, T>) {}

    #[inline(always)]
    fn sync_before_exec<T: EvmTypes>(
        &self,
        _state: &mut InterpreterState<'_, T>,
        _dynamic_gas: bool,
    ) {
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypes>(
        &mut self,
        _state: &mut InterpreterState<'_, T>,
        _dynamic_gas: bool,
    ) {
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_inner<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>, G: DispatchGas>(
    mut pc: Pc,
    mut stack: StackMut<'_>,
    mut gas: G,
    state: &mut InterpreterState<'_, T>,
    op: u8,
) -> (Pc, G) {
    if M::INSPECT {
        gas.sync_before_inspect(state);
        M::step(state, pc, stack.len());
        if state.result().is_err() {
            return (pc, gas);
        }
    }

    let instruction = C::OPCODE_CONFIG.instruction(op);
    let instr = instruction.instr;
    let dynamic_gas = instruction.dynamic_gas;
    let r;
    match gas.pre_step::<T, C>(state, op) {
        Ok(()) => {
            gas.sync_before_exec(state, dynamic_gas);
            r = instr(&mut pc, stack.reborrow(), state);
            if r.is_ok() {
                inc_pc(&mut pc, op);
            }
            gas.sync_after_exec(state, dynamic_gas);
        }
        Err(e) => {
            gas.sync_before_exec(state, false);
            r = Err(e);
        }
    }
    if M::INSPECT {
        state.set_result(r);
        gas.sync_before_inspect(state);
        M::step_end(state, pc, stack.len());
    } else if let Err(e) = r {
        state.set_result(Err(e));
        cold_path();
        if state.inspect_steps_in_loop() {
            return (pc, gas);
        }
        return (Pc::new(core::ptr::null()), gas);
    }
    (pc, gas)
}

pub(in crate::interpreter) fn run<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let (state, pc, stack) = run_state(interpreter);
    if state.is_inspecting() {
        if state.inspect_steps_in_loop() {
            return run_inner::<T, true, true>(state, pc, stack, instructions);
        }
        return run_inner::<T, true, false>(state, pc, stack, instructions);
    }
    run_inner::<T, false, false>(state, pc, stack, instructions)
}

#[allow(clippy::let_unit_value)]
fn run_inner<T: EvmTypes, const INSPECTING: bool, const LOOP_INSPECT: bool>(
    state: &mut InterpreterState<'_, T>,
    mut pc: Pc,
    mut stack: Stack<'_>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let mut loop_state = imp::loop_state(state.gas_mut());
    loop {
        let op = pc.op();
        let instr = instructions[op as usize];
        if LOOP_INSPECT {
            imp::sync_loop_state(state, loop_state);
            <DynInspector as InspectMode<T>>::step(state, pc, stack.len);
            if state.result().is_err() {
                return finish_run(state, pc, stack.len, loop_state);
            }
        }
        let (next_pc, next_stack_len) =
            imp::dispatch_loop_call(instr, pc, stack.reborrow(), state, &mut loop_state);
        pc = next_pc;
        stack.len = next_stack_len;

        if LOOP_INSPECT {
            imp::sync_loop_state(state, loop_state);
            <DynInspector as InspectMode<T>>::step_end(state, pc, stack.len);
            if state.result().is_err() {
                return finish_run(state, pc, stack.len, loop_state);
            }
        } else if INSPECTING {
            imp::sync_loop_state(state, loop_state);
            if state.result().is_err() {
                return finish_run(state, pc, stack.len, loop_state);
            }
        } else if pc.as_ptr().is_null() {
            return finish_run(state, pc, stack.len, loop_state);
        }
    }
}

#[inline(always)]
fn finish_run<T: EvmTypes>(
    state: &mut InterpreterState<'_, T>,
    pc: Pc,
    stack_len: usize,
    loop_state: imp::LoopState,
) -> InstrStop {
    cold_path();
    state.set_pc_stack_len(pc.as_ptr(), stack_len);
    imp::finish_loop(state.gas_mut(), loop_state);
    state.result().unwrap_err()
}
