use super::{DynInspector, InspectMode, NoInspector, inc_pc, run_state};
use crate::{
    EvmConfig, EvmTypesHost,
    interpreter::{
        InstrStop, Interpreter, InterpreterState, Pc, RawStack, Result, StackMut, fused,
    },
};
use core::hint::cold_path;

core::cfg_select! {
    dispatch_packed => {
        mod packed;
        use packed as imp;
    }
    dispatch_single_return => {
        mod single_return;
        use single_return as imp;
    }
    _ => {
        mod unpacked;
        use unpacked as imp;
    }
}

pub(super) use imp::{RawInstrFn, dispatch};

/// Table instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

/// Program counter and stack length carried between instructions by the dispatch loop.
trait Cursor: Copy {
    fn new(pc: Pc, stack_len: usize) -> Self;

    fn pc(self) -> Pc;

    fn stack_len(self) -> usize;
}

impl Cursor for (Pc, usize) {
    #[inline(always)]
    fn new(pc: Pc, stack_len: usize) -> Self {
        (pc, stack_len)
    }

    #[inline(always)]
    fn pc(self) -> Pc {
        self.0
    }

    #[inline(always)]
    fn stack_len(self) -> usize {
        self.1
    }
}

trait DispatchGas: Copy {
    fn pre_step<T: EvmTypesHost, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        op: u8,
    ) -> Result;

    fn sync_before_exec<T: EvmTypesHost>(
        &self,
        state: &mut InterpreterState<'_, '_, T>,
        dynamic_gas: bool,
    );

    /// Undoes a failed `pre_step` that charged `cost`.
    fn undo_spend<T: EvmTypesHost>(&mut self, state: &mut InterpreterState<'_, '_, T>, cost: u64);

    fn sync_after_exec<T: EvmTypesHost>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        dynamic_gas: bool,
    );
}

impl DispatchGas for () {
    #[inline(always)]
    fn pre_step<T: EvmTypesHost, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        op: u8,
    ) -> Result {
        state.gas_mut().spend(C::OPCODE_CONFIG.static_gas(op) as _)
    }

    #[inline(always)]
    fn undo_spend<T: EvmTypesHost>(&mut self, state: &mut InterpreterState<'_, '_, T>, cost: u64) {
        let gas = state.gas_mut();
        gas.set_remaining(gas.remaining().wrapping_add(cost));
    }

    #[inline(always)]
    fn sync_before_exec<T: EvmTypesHost>(
        &self,
        _state: &mut InterpreterState<'_, '_, T>,
        _dynamic_gas: bool,
    ) {
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypesHost>(
        &mut self,
        _state: &mut InterpreterState<'_, '_, T>,
        _dynamic_gas: bool,
    ) {
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_inner<T: EvmTypesHost, C: EvmConfig<T>, M: InspectMode<T>, G: DispatchGas>(
    mut pc: Pc,
    mut stack: StackMut<'_>,
    mut gas: G,
    state: &mut InterpreterState<'_, '_, T>,
    op: u8,
) -> (Pc, G) {
    let instruction = C::OPCODE_CONFIG.instruction(op);
    let instr = instruction.instr;
    let dynamic_gas = instruction.dynamic_gas;
    let r;
    match gas.pre_step::<T, C>(state, op) {
        Ok(()) => {
            gas.sync_before_exec(state, dynamic_gas);
            r = instr(&mut pc, stack.reborrow(), state);
            if r.is_ok() {
                inc_pc::<T, M>(&mut pc, op);
            }
            gas.sync_after_exec(state, dynamic_gas);
        }
        Err(e) => {
            if M::FUSED
                && let Some(first) = fused::unfused_op(op)
            {
                // Not enough gas for the whole sequence: run its first opcode alone.
                cold_path();
                gas.undo_spend(state, C::OPCODE_CONFIG.static_gas(op) as u64);
                return dispatch_unfused::<T, C, M, G>(pc, stack, gas, state, first);
            }
            gas.sync_before_exec(state, false);
            r = Err(e);
        }
    }
    if M::INSPECT {
        state.set_result(r);
    } else if let Err(e) = r {
        state.set_result(Err(e));
        cold_path();
        return (Pc::new(core::ptr::null()), gas);
    }
    (pc, gas)
}

#[cold]
#[inline(never)]
fn dispatch_unfused<T: EvmTypesHost, C: EvmConfig<T>, M: InspectMode<T>, G: DispatchGas>(
    pc: Pc,
    stack: StackMut<'_>,
    gas: G,
    state: &mut InterpreterState<'_, '_, T>,
    op: u8,
) -> (Pc, G) {
    dispatch_inner::<T, C, M, G>(pc, stack, gas, state, op)
}

#[inline(always)]
pub(in crate::interpreter) fn run<T: EvmTypesHost>(
    interpreter: &mut Interpreter<'_, '_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let (state, pc, stack) = run_state(interpreter);
    if state.is_inspecting() {
        return run_inner::<T, DynInspector>(state, pc, stack, instructions);
    }
    run_inner::<T, NoInspector>(state, pc, stack, instructions)
}

#[allow(clippy::let_unit_value)]
fn run_inner<T: EvmTypesHost, M: InspectMode<T>>(
    state: &mut InterpreterState<'_, '_, T>,
    pc: Pc,
    mut stack: RawStack<'_>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let mut loop_state = imp::loop_state(state.gas_mut());
    let mut cursor = imp::Cursor::new(pc, stack.len());
    loop {
        if M::INSPECT {
            imp::sync_loop_state(state, loop_state);
            M::step(state, cursor.pc(), cursor.stack_len());
            loop_state = imp::loop_state(state.gas_mut());
            if state.result().is_err() {
                return finish_run(state, cursor, loop_state);
            }
        }

        let op = cursor.pc().op();
        let instr = instructions[op as usize];
        cursor = {
            // SAFETY: This is the only stack borrow outside inspector callbacks.
            let stack_view = unsafe { stack.borrow() };
            imp::dispatch_loop_call(instr, cursor, stack_view.stack, state, &mut loop_state)
        };

        if M::INSPECT {
            imp::sync_loop_state(state, loop_state);
            M::step_end(state, cursor.pc(), cursor.stack_len());
            loop_state = imp::loop_state(state.gas_mut());
            if state.result().is_err() {
                return finish_run(state, cursor, loop_state);
            }
        } else if cursor.pc().as_ptr().is_null() {
            return finish_run(state, cursor, loop_state);
        }
    }
}

#[inline(always)]
fn finish_run<T: EvmTypesHost>(
    state: &mut InterpreterState<'_, '_, T>,
    cursor: imp::Cursor,
    loop_state: imp::LoopState,
) -> InstrStop {
    cold_path();
    let (pc, stack_len) = (cursor.pc(), cursor.stack_len());
    // The NULL `pc` is only a loop-exit sentinel; never publish it.
    let pc = if pc.as_ptr().is_null() { state.bytecode().as_ptr() } else { pc.as_ptr() };
    state.set_pc_stack_len(pc, stack_len);
    imp::finish_loop(state.gas_mut(), loop_state);
    state.result().unwrap_err()
}
