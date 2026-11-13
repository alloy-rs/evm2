use super::{FusedStep, InspectMode, NoInspector, run_state};
use crate::{
    EvmConfig, EvmTypesHost,
    interpreter::{
        InstrStop, Interpreter, InterpreterState, Pc, RawStack, Result,
        gas::RemainingGas,
        private::{InstructionFlags, InstructionImplFn},
    },
};
use core::hint::cold_path;

/// Tail instruction function pointer.
type TailInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: RawStack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, '_, T>,
        instructions: *const (),
    )
);

/// Tail instruction dispatch table.
type TailInstrTable<T> = [TailInstrFn<T>; 256];

pub(super) type RawInstrFn<T> = TailInstrFn<T>;

pub(super) type RawInstrTable<T> = TailInstrTable<T>;

#[inline(always)]
pub(in crate::interpreter) fn run<T: EvmTypesHost>(
    interpreter: &mut Interpreter<'_, '_, T>,
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
    // Without alignment, each handler's offset inside its 64-byte line depends on unrelated code
    // size. Aligned handlers run faster and vary less across layouts.
    #[cfg_attr(target_arch = "x86_64", rustc_align(64))]
    pub(super) fn dispatch<
        T: EvmTypesHost,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const OP: u8,
    >(
        mut pc: Pc,
        mut stack: RawStack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, '_, T>,
        instructions: *const (),
    ) {
        let instruction = C::OPCODE_CONFIG.instruction(OP);
        let instr: InstructionImplFn<T> = instruction.instr;
        let dynamic_gas = instruction.flags.contains(InstructionFlags::DYNAMIC_GAS);
        let hooked = instruction.flags.contains(InstructionFlags::HOOKED);
        if M::INSPECT {
            // Handlers that skip the hooks keep the gas in a register.
            state.gas_mut().set_remaining(remaining_gas.get());
            M::step(state, pc, stack.len());
            remaining_gas.set(state.gas_mut().remaining());
            if state.result().is_err() {
                cold_path();
                tail_return!(tail_call_restore(pc, stack, remaining_gas, state, instructions));
            }
        }
        if let Err(e) = pre_step::<T, C>(&mut remaining_gas, OP) {
            cold_path();
            state.set_result(Err(e));
            if M::INSPECT {
                state.gas_mut().set_remaining(remaining_gas.get());
                M::step_end(state, pc, stack.len());
                remaining_gas.set(state.gas_mut().remaining());
            }
            tail_return!(tail_call_restore(pc, stack, remaining_gas, state, instructions));
        }
        if M::INSPECT || dynamic_gas {
            state.gas_mut().set_remaining(remaining_gas.get());
        }
        if hooked {
            state.set_pc_stack_len(pc.as_ptr(), stack.len());
        }
        let (r, stack_len) = {
            // SAFETY: This is the only stack borrow outside inspector callbacks.
            let mut stack_view = unsafe { stack.borrow() };
            let r = instr(&mut pc, stack_view.as_mut(), state);
            (r, stack_view.len)
        };
        stack.set_len(stack_len);
        if dynamic_gas {
            remaining_gas.set(state.gas_mut().remaining());
        }
        if let Err(e) = r {
            cold_path();
            state.set_result(Err(e));
            if M::INSPECT {
                M::step_end(state, pc, stack.len());
                remaining_gas.set(state.gas_mut().remaining());
            }
            tail_return!(tail_call_restore(pc, stack, remaining_gas, state, instructions));
        }
        super::inc_pc(&mut pc, OP);
        if hooked {
            state.note_inspector_config_change();
            if !M::INSPECT && state.reload_requested() {
                // The frame continues after the instruction on the table for the new config.
                cold_path();
                state.set_result(Err(InstrStop::Reload));
                tail_return!(tail_call_restore(pc, stack, remaining_gas, state, instructions));
            }
        }
        if M::INSPECT {
            M::step_end(state, pc, stack.len());
            remaining_gas.set(state.gas_mut().remaining());
            if state.result().is_err() {
                cold_path();
                tail_return!(tail_call_restore(pc, stack, remaining_gas, state, instructions));
            }
        }
        // SAFETY: `instructions` is a pointer to a `TailInstrTable`.
        let instructions_t = unsafe { &*instructions.cast::<TailInstrTable<T>>() };
        let instr = instructions_t[pc.op() as usize];
        tail_return!(instr(pc, stack, remaining_gas, state, instructions));
    }
}

extern_table! {
    /// Runs the instructions `A`, `B`, `D` and `E`, of which the first `N` are real, like their
    /// own handlers would, and dispatches once after the last.
    #[cfg_attr(target_arch = "x86_64", rustc_align(64))]
    pub(super) fn dispatch_fused<
        T: EvmTypesHost,
        C: EvmConfig<T>,
        const N: usize,
        const A: u8,
        const B: u8,
        const D: u8,
        const E: u8,
    >(
        mut pc: Pc,
        mut stack: RawStack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, '_, T>,
        instructions: *const (),
    ) {
        macro_rules! step {
            ($index:literal, $op:ident) => {
                if N > $index {
                    let step = const { FusedStep::<T>::new::<C>([A, B, D, E], N, $index) };
                    if let Some(group_gas) = step.group_gas
                        && remaining_gas.get() < group_gas
                    {
                        // One of the next instructions runs out of gas. Their own handlers find
                        // which one.
                        cold_path();
                        if $index == 0 {
                            tail_return!(dispatch::<T, C, NoInspector, A>(
                                pc,
                                stack,
                                remaining_gas,
                                state,
                                instructions,
                            ));
                        }
                        // SAFETY: `instructions` is a pointer to a `TailInstrTable`.
                        let instructions_t = unsafe { &*instructions.cast::<TailInstrTable<T>>() };
                        let instr = instructions_t[pc.op() as usize];
                        tail_return!(instr(pc, stack, remaining_gas, state, instructions));
                    }
                    remaining_gas.set(remaining_gas.get().wrapping_sub(step.static_gas));
                    if step.dynamic_gas {
                        state.gas_mut().set_remaining(remaining_gas.get());
                    }
                    let (r, stack_len) = {
                        // SAFETY: This is the only stack borrow outside inspector callbacks.
                        let mut stack_view = unsafe { stack.borrow() };
                        let r = (step.instr)(&mut pc, stack_view.as_mut(), state);
                        (r, stack_view.len)
                    };
                    stack.set_len(stack_len);
                    if step.dynamic_gas {
                        remaining_gas.set(state.gas_mut().remaining());
                    }
                    if let Err(e) = r {
                        cold_path();
                        state.set_result(Err(e));
                        tail_return!(tail_call_restore(
                            pc,
                            stack,
                            remaining_gas,
                            state,
                            instructions,
                        ));
                    }
                    super::inc_pc(&mut pc, $op);
                }
            };
        }

        step!(0, A);
        step!(1, B);
        step!(2, D);
        step!(3, E);
        // SAFETY: `instructions` is a pointer to a `TailInstrTable`.
        let instructions_t = unsafe { &*instructions.cast::<TailInstrTable<T>>() };
        let instr = instructions_t[pc.op() as usize];
        tail_return!(instr(pc, stack, remaining_gas, state, instructions));
    }
}

extern_table! {
    #[inline(never)]
    #[cold]
    fn tail_call_restore<T: EvmTypesHost>(
        pc: Pc,
        stack: RawStack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, '_, T>,
        _instructions: *const (),
    ) {
        state.gas_mut().set_remaining(remaining_gas.get());
        state.set_pc_stack_len(pc.as_ptr(), stack.len());
        debug_assert!(state.result().is_err());
        // Exits by returning normally.
    }
}

#[inline(always)]
const fn pre_step<T: EvmTypesHost, C: EvmConfig<T>>(
    remaining_gas: &mut RemainingGas,
    opcode: u8,
) -> Result {
    remaining_gas.spend(C::OPCODE_CONFIG.static_gas(opcode) as _)
}
