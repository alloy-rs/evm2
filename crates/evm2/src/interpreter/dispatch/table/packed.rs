use crate::{
    EvmConfig, EvmTypes,
    interpreter::{
        InterpreterState, Pc, Result, Stack,
        gas::{Gas, RemainingGas},
    },
};

pub(super) type LoopState = RemainingGas;

/// Packed instruction return value.
pub(crate) type InstrFnRet = (Pc, usize);

/// Packed instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: &mut RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet
);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    remaining_gas: &mut LoopState,
) -> (Pc, usize) {
    instr(pc, stack, remaining_gas, state)
}

#[inline(always)]
pub(super) const fn loop_state(gas: &Gas) -> LoopState {
    RemainingGas::new(gas.remaining())
}

#[inline(always)]
pub(super) const fn finish_loop(gas: &mut Gas, remaining_gas: LoopState) {
    gas.set_remaining(remaining_gas.get());
}

#[inline(always)]
pub(super) const fn sync_loop_state<T: EvmTypes>(
    state: &mut InterpreterState<'_, T>,
    loop_state: LoopState,
) {
    state.gas_mut().set_remaining(loop_state.get());
}

impl super::DispatchGas for &mut RemainingGas {
    #[inline(always)]
    fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
        &mut self,
        _state: &mut InterpreterState<'_, T>,
        op: u8,
    ) -> Result {
        (*self).spend(C::VERSION_TABLES.static_gas(op) as _)
    }

    #[inline(always)]
    fn sync_before_exec<T: EvmTypes>(
        &self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
    ) {
        if dynamic_gas {
            state.gas_mut().set_remaining((**self).get());
        }
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypes>(
        &mut self,
        state: &mut InterpreterState<'_, T>,
        dynamic_gas: bool,
    ) {
        if dynamic_gas {
            (*self).set(state.gas_mut().remaining());
        }
    }
}

extern_table! {
    pub(in crate::interpreter::dispatch) fn dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: super::InspectMode<T>,
        const OP: u8,
    >(
        pc: Pc,
        mut stack: Stack<'_>,
        remaining_gas: &mut RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        let (pc, _) =
            super::dispatch_inner::<T, C, M, _>(
                pc,
                stack.as_mut(),
                remaining_gas,
                state,
                OP,
            );
        (pc, stack.len)
    }
}
