use super::{DynInspector, InspectMode, NoInspector};
use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::RemainingGas},
};
use core::hint::cold_path;

/// Tail instruction function pointer.
type TailInstrFn<T> = extern_table!(
    fn(pc: Pc, stack: Stack<'_>, remaining_gas: RemainingGas, state: &mut InterpreterState<'_, T>)
);

/// Tail instruction dispatch table.
type TailInstrTable<T> = [TailInstrFn<T>; 256];

pub(super) type RawInstrFn<T> = TailInstrFn<T>;

pub(super) type RawInstrTable<T> = TailInstrTable<T>;

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $evm_types:ty, $config:ty, $mode:ty, $vt:ident, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            if !$vt.is_unknown_opcode($op) {
                $table[$op] = if $vt.instruction($op).dynamic_gas {
                    $dispatch::<$evm_types, $config, $mode, $op, true, false> as $instr_fn
                } else {
                    $dispatch::<$evm_types, $config, $mode, $op, false, false> as $instr_fn
                };
            }
        )*
    };
}

pub(crate) const fn make_table<T, C, M>() -> RawInstrTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
    M: TailInspectMode<T>,
{
    let mut table = [tail_dispatch::<T, C, M, 0xFE, false, true> as super::InstrFn<T>; 256];
    let vt = C::VERSION_TABLES;
    for_each_opcode_value!([table, T, C, M, vt, tail_dispatch, super::InstrFn<T>] assign_instruction_table_entries);
    table
}

extern_table! {
    #[optimize(none)]
    fn tail_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: TailInspectMode<T>,
        const OP: u8,
        const DYNAMIC_GAS: bool,
        const UNKNOWN: bool,
    >(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        if !UNKNOWN {
            unsafe { core::hint::assert_unchecked(pc.op() == OP) };
        }
        tail_return!(tail_dispatch_mono::<T, C, M, DYNAMIC_GAS, UNKNOWN>(
            pc,
            stack,
            remaining_gas,
            state
        ));
    }
}

extern_table! {
    #[inline(always)]
    fn tail_dispatch_mono<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: TailInspectMode<T>,
        const DYNAMIC_GAS: bool,
        const UNKNOWN: bool,
    >(
        mut pc: Pc,
        mut stack: Stack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        let (op, instr) = if UNKNOWN {
            (0xFE, super::unknown_instruction as crate::interpreter::private::InstructionImplFn<T>)
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
            tail_return!(tail_call_restore::<T>(pc, stack, remaining_gas, state));
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
            tail_return!(tail_call_restore::<T>(pc, stack, remaining_gas, state));
        }
        super::inc_pc(&mut pc, op);
        if M::INSPECT {
            M::step_end(state, pc, stack.len);
        }
        let instr = M::next::<C>(pc.op());
        tail_return!(instr(pc, stack, remaining_gas, state));
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
    ) {
        state.gas_mut().set_remaining(remaining_gas.get());
        state.set_pc_stack_len(pc.as_ptr(), stack.len);
        debug_assert!(state.result().is_err());
        // Exits by returning normally.
    }
}

#[inline]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
    remaining_gas: &mut RemainingGas,
    op: u8,
) -> Result {
    remaining_gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}

pub(super) trait TailInspectMode<T: EvmTypes>: InspectMode<T> {
    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T>;
}

impl<T: EvmTypes> TailInspectMode<T> for NoInspector {
    #[inline(always)]
    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T> {
        <T as super::InstrTables<C>>::INSTRUCTIONS[op as usize]
    }
}

impl<T: EvmTypes> TailInspectMode<T> for DynInspector {
    #[inline(always)]
    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T> {
        <T as super::InstrTables<C>>::INSPECT_INSTRUCTIONS[op as usize]
    }
}
