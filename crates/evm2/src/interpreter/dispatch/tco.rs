use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::RemainingGas},
};
use core::hint::cold_path;

/// Tail instruction function pointer.
type TailInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        instructions: *const (),
    )
);

/// Tail instruction dispatch table.
type TailInstrTable<T> = [TailInstrFn<T>; 256];

pub(super) type RawInstrFn<T> = TailInstrFn<T>;

pub(super) type RawInstrTable<T> = TailInstrTable<T>;

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
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        instructions: *const (),
    ) {
        unsafe { core::hint::assert_unchecked(pc.op() == OP) };
        tail_return!(tail_dispatch_mono::<T, C, M, DYNAMIC_GAS, false>(
            pc,
            stack,
            remaining_gas,
            state,
            instructions
        ));
    }
}

extern_table! {
    pub(super) fn unknown_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
    >(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        instructions: *const (),
    ) {
        tail_return!(tail_dispatch_mono::<T, C, M, false, true>(
            pc,
            stack,
            remaining_gas,
            state,
            instructions
        ));
    }
}

extern_table! {
    #[inline(always)]
    fn tail_dispatch_mono<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const DYNAMIC_GAS: bool,
        const UNKNOWN: bool,
    >(
        mut pc: Pc,
        mut stack: Stack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        instructions: *const (),
    ) {
        let (op, instr) = if UNKNOWN {
            (
                pc.op(),
                super::unknown_instruction as crate::interpreter::private::InstructionImplFn<T>,
            )
        } else {
            let op = pc.op();
            (op, C::VERSION_TABLES.instruction(op).instr)
        };
        if M::INSPECT {
            M::step(state, pc, stack.len);
        }
        if let Err(e) = pre_step::<T, C>(&mut remaining_gas, op) {
            cold_path();
            state.set_result(Err(e));
            if M::INSPECT {
                M::step_end(state, pc, stack.len);
            }
            tail_return!(tail_call_restore::<T>(pc, stack, remaining_gas, state, instructions));
        }
        if DYNAMIC_GAS {
            state.gas_mut().set_remaining(remaining_gas.get());
        }
        let r = instr(&mut pc, stack.as_mut(), state);
        if DYNAMIC_GAS {
            remaining_gas.set(state.gas_mut().remaining());
        }
        if let Err(e) = r {
            cold_path();
            state.set_result(Err(e));
            if M::INSPECT {
                M::step_end(state, pc, stack.len);
            }
            tail_return!(tail_call_restore::<T>(pc, stack, remaining_gas, state, instructions));
        }
        super::inc_pc(&mut pc, op);
        if M::INSPECT {
            M::step_end(state, pc, stack.len);
        }
        let instructions = instructions.cast::<TailInstrTable<T>>();
        let instr = unsafe { (*instructions)[pc.op() as usize] };
        tail_return!(instr(pc, stack, remaining_gas, state, instructions.cast()));
    }
}

extern_table! {
    #[inline(never)] // TODO
    #[cold]
    fn tail_call_restore<T: EvmTypes>(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        _instructions: *const (),
    ) {
        state.gas_mut().set_remaining(remaining_gas.get());
        state.set_pc_stack_len(pc.as_ptr(), stack.len);
        debug_assert!(state.result().is_err());
        // Exits by returning normally.
    }
}

#[inline(always)]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
    remaining_gas: &mut RemainingGas,
    op: u8,
) -> Result {
    remaining_gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}
