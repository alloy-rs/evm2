use crate::{
    EvmConfig, EvmTypes,
    evm::inspector::Inspector,
    interpreter::{InstrStop, InterpreterState, Pc, Result, Stack, gas::RemainingGas},
};
use core::{hint::cold_path, marker::PhantomData};

/// Tail instruction function pointer.
type TailInstrFn<T> = extern_table!(
    fn(pc: Pc, stack: Stack<'_>, remaining_gas: RemainingGas, state: &mut InterpreterState<'_, T>)
);

/// Tail instruction dispatch table.
type TailInstrTable<T> = [TailInstrFn<T>; 256];

pub(super) type RawInstrFn<T> = TailInstrFn<T>;

pub(super) type RawInstrTable<T> = TailInstrTable<T>;

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $evm_types:ty, $config:ty, $mode:ty, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            let instruction = <$config as EvmConfig<$evm_types>>::VERSION_TABLES.instruction($op);
            $table[$op] = if instruction.dynamic_gas {
                $dispatch::<$evm_types, $config, $mode, $op, true> as $instr_fn
            } else {
                $dispatch::<$evm_types, $config, $mode, $op, false> as $instr_fn
            };
        )*
    };
}

pub(crate) const fn make_instruction_table<T, C>() -> RawInstrTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
{
    let mut table = [tail_dispatch::<T, C, NoInspector, 0, true> as super::InstrFn<T>; 256];
    for_each_opcode_value!([table, T, C, NoInspector, tail_dispatch, super::InstrFn<T>] assign_instruction_table_entries);

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

pub(crate) const fn make_inspect_instruction_table<T, C>() -> RawInstrTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
{
    let mut table = [tail_dispatch::<T, C, DynInspector, 0, true> as super::InstrFn<T>; 256];
    for_each_opcode_value!([table, T, C, DynInspector, tail_dispatch, super::InstrFn<T>] assign_instruction_table_entries);

    table
}

pub(crate) const fn make_typed_inspect_instruction_table<T, C, I>() -> RawInstrTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
    I: Inspector<T>,
{
    let mut table = [tail_dispatch::<T, C, TypedInspector<I>, 0, true> as super::InstrFn<T>; 256];
    for_each_opcode_value!([table, T, C, TypedInspector<I>, tail_dispatch, super::InstrFn<T>] assign_instruction_table_entries);

    table
}

extern_table! {
    fn tail_dispatch<
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
    ) {
        assume!(pc.op() == OP);
        tail_return!(tail_dispatch_mono::<T, C, M, DYNAMIC_GAS>(
            pc,
            stack,
            remaining_gas,
            state
        ));
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
    fn tail_dispatch_mono<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const DYNAMIC_GAS: bool,
    >(
        mut pc: Pc,
        mut stack: Stack<'_>,
        mut remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) {
        let op = pc.op();
        let instr = C::VERSION_TABLES.instruction(op).instr;
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

trait InspectMode<T: EvmTypes> {
    const INSPECT: bool;

    fn step(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize);

    fn step_end(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize);

    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T>;
}

struct NoInspector;

impl<T: EvmTypes> InspectMode<T> for NoInspector {
    const INSPECT: bool = false;

    #[inline(always)]
    fn step(_state: &mut InterpreterState<'_, T>, _pc: Pc, _stack_len: usize) {}

    #[inline(always)]
    fn step_end(_state: &mut InterpreterState<'_, T>, _pc: Pc, _stack_len: usize) {}

    #[inline(always)]
    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T> {
        <T as super::InstrTables<C>>::INSTRUCTIONS[op as usize]
    }
}

struct DynInspector;

impl<T: EvmTypes> InspectMode<T> for DynInspector {
    const INSPECT: bool = true;

    #[inline(always)]
    fn step(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step(pc, stack_len);
    }

    #[inline(always)]
    fn step_end(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step_end(pc, stack_len);
    }

    #[inline(always)]
    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T> {
        <T as super::InstrTables<C>>::INSPECT_INSTRUCTIONS[op as usize]
    }
}

struct TypedInspector<I>(PhantomData<fn() -> I>);

impl<T, I> InspectMode<T> for TypedInspector<I>
where
    T: EvmTypes,
    I: Inspector<T>,
{
    const INSPECT: bool = true;

    #[inline(always)]
    fn step(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step_as::<I>(pc, stack_len);
    }

    #[inline(always)]
    fn step_end(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step_end_as::<I>(pc, stack_len);
    }

    #[inline(always)]
    fn next<C: EvmConfig<T>>(op: u8) -> TailInstrFn<T> {
        <T as super::TypedInspectInstrTables<C, I>>::INSPECT_INSTRUCTIONS[op as usize]
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
