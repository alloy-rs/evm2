use super::{InspectMode, run_state};
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{
        InstrStop, Interpreter, InterpreterState, Pc, Result, Stack, gas::RemainingGas,
        private::InstructionImplFn,
    },
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

#[inline(always)]
pub(in crate::interpreter) fn run<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let remaining_gas = RemainingGas::new(interpreter.gas.remaining());
    let (state, pc, stack) = run_state(interpreter);
    let op = pc.op();
    let instr = instructions[op as usize];
    instr(pc, stack, remaining_gas, state, (instructions as *const RawInstrTable<T>).cast());
    state.result().unwrap_err()
}

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
        tail_return!(tail_dispatch_mono::<T, C, M, DYNAMIC_GAS, false, OP>(
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
        tail_return!(tail_dispatch_mono::<T, C, M, false, true, { super::UNKNOWN_OP }>(
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
        const OP: u8,
    >(
        mut pc: Pc,
        mut stack: Stack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        instructions: *const (),
    ) {
        if !UNKNOWN {
            unsafe { core::hint::assert_unchecked(pc.op() == OP) };
        }
        let instr: InstructionImplFn<T> = if UNKNOWN {
            super::unknown_instruction
        } else {
            C::OPCODE_TABLES.instruction(OP).instr
        };
        if M::INSPECT {
            M::step(state, pc, stack.len);
        }
        if let Err(e) = pre_step::<T, C, OP>(&mut remaining_gas) {
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
        super::inc_pc(&mut pc, OP);
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
const fn pre_step<T: EvmTypes, C: EvmConfig<T>, const OP: u8>(
    remaining_gas: &mut RemainingGas,
) -> Result {
    remaining_gas.spend(C::OPCODE_TABLES.static_gas(OP) as _)
}
