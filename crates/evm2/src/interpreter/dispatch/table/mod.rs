use super::{InspectMode, UNKNOWN_OP, inc_pc, run_state, unknown_instruction};
#[cfg(dispatch_packed)]
use crate::constants::STACK_LIMIT;
#[cfg(dispatch_packed)]
use crate::interpreter::gas::RemainingGas;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InstrStop, Interpreter, InterpreterState, Pc, Result, Stack, gas::Gas},
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

trait DispatchGas: Copy {
    fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        op: u8,
    ) -> Result;

    fn sync_before_exec<T: EvmTypes>(
        &self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
        inspect: bool,
    );

    fn sync_after_exec<T: EvmTypes>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
    );

    fn error_stack_len(stack_len: usize) -> usize;
}

impl DispatchGas for () {
    #[inline(always)]
    fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        op: u8,
    ) -> Result {
        state.gas_mut().spend(C::VERSION_TABLES.static_gas(op) as _)
    }

    #[inline(always)]
    fn sync_before_exec<T: EvmTypes>(
        &self,
        _state: &mut InterpreterState<'_, T>,
        _dynamic_gas: bool,
        _inspect: bool,
    ) {
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypes>(
        &mut self,
        _state: &mut InterpreterState<'_, T>,
        _dynamic_gas: bool,
    ) {
    }

    #[inline(always)]
    fn error_stack_len(stack_len: usize) -> usize {
        stack_len
    }
}

#[cfg(dispatch_packed)]
impl DispatchGas for RemainingGas {
    #[inline(always)]
    fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
        &mut self,
        _state: &mut InterpreterState<'_, T>,
        op: u8,
    ) -> Result {
        self.spend(C::VERSION_TABLES.static_gas(op) as _)
    }

    #[inline(always)]
    fn sync_before_exec<T: EvmTypes>(
        &self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
        inspect: bool,
    ) {
        if inspect || dynamic_gas {
            state.gas_mut().set_remaining(self.get());
        }
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypes>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
    ) {
        if dynamic_gas {
            self.set(state.gas_mut().remaining());
        }
    }

    #[inline(always)]
    fn error_stack_len(stack_len: usize) -> usize {
        if stack_len <= STACK_LIMIT { stack_len } else { 0 }
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_inner<
    T: EvmTypes,
    C: EvmConfig<T>,
    M: InspectMode<T>,
    G: DispatchGas,
    const DYNAMIC_GAS: bool,
    const UNKNOWN: bool,
>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    mut gas: G,
    state: &mut InterpreterState<'_, T>,
    op: u8,
) -> (Pc, G, usize) {
    let instr = if UNKNOWN { unknown_instruction } else { C::VERSION_TABLES.instruction(op).instr };
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match gas.pre_step::<T, C>(state, op) {
        Ok(()) => {
            gas.sync_before_exec(state, DYNAMIC_GAS, M::INSPECT);
            r = instr(&mut pc, stack.as_mut(), state);
            gas.sync_after_exec(state, DYNAMIC_GAS);
            if !M::INSPECT || r.is_ok() {
                inc_pc(&mut pc, op);
            }
        }
        Err(e) => {
            gas.sync_before_exec(state, false, M::INSPECT);
            r = Err(e);
        }
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
        return (Pc::new(core::ptr::null()), gas, G::error_stack_len(stack.len));
    }
    (pc, gas, stack.len)
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
