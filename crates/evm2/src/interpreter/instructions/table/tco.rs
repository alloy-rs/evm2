use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InstrStop, InterpreterState, Pc, Result, Stack, gas::RemainingGas},
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
    ([$table:expr, $evm_types:ty, $config:ty, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            let instruction = <$config as EvmConfig<$evm_types>>::VERSION_TABLES.instruction($op);
            $table[$op] = if instruction.dynamic_gas {
                $dispatch::<$evm_types, $config, $op, true> as $instr_fn
            } else {
                $dispatch::<$evm_types, $config, $op, false> as $instr_fn
            };
        )*
    };
}

pub(crate) const fn make_instruction_table<T, C>() -> RawInstrTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
{
    use tail_dispatch as dispatch;

    let mut table = [dispatch::<T, C, 0, true> as super::InstrFn<T>; 256];
    for_each_opcode_value!([table, T, C, dispatch, super::InstrFn<T>] assign_instruction_table_entries);

    // Make all unknown entries point to the same dispatch function.
    let mut i = 0;
    while i < 256 {
        if C::VERSION_TABLES.is_unknown_opcode(i as u8) {
            table[i] = tail_unknown_dispatch::<T, C> as super::InstrFn<T>;
        }
        i += 1;
    }

    table
}

extern_table! {
    fn tail_dispatch<T: EvmTypes, C: EvmConfig<T>, const OP: u8, const DYNAMIC_GAS: bool>(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        assume!(pc.op() == OP);
        tail_return!(tail_dispatch_mono::<T, C, DYNAMIC_GAS>(pc, stack, remaining_gas, state));
    }
}

extern_table! {
    #[cold]
    fn tail_unknown_dispatch<T: EvmTypes, C: EvmConfig<T>>(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        assume!(C::VERSION_TABLES.is_unknown_opcode(pc.op()));
        state.set_result(Err(InstrStop::OpcodeNotFound));
        tail_return!(tail_call_restore::<T>(pc, stack, remaining_gas, state));
    }
}

extern_table! {
    #[inline(always)]
    fn tail_dispatch_mono<T: EvmTypes, C: EvmConfig<T>, const DYNAMIC_GAS: bool>(
        mut pc: Pc,
        mut stack: Stack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        let op = pc.op();
        let instr = C::VERSION_TABLES.instruction(op).instr;
        if let Err(e) = pre_step::<T, C>(&mut remaining_gas, op) {
            cold_path();
            state.set_result(Err(e));
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
            tail_return!(tail_call_restore::<T>(pc, stack, remaining_gas, state));
        }
        super::inc_pc(&mut pc, op);
        tail_return!(tail_call_next::<T, C>(pc, stack, remaining_gas, state));
    }
}

extern_table! {
    #[inline]
    fn tail_call_next<T: EvmTypes, C: EvmConfig<T>>(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        let instr = <T as super::InstrTables<C>>::INSTRUCTIONS[pc.op() as usize];
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
