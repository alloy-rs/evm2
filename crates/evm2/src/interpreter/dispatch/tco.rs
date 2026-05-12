use super::InspectMode;
use crate::{
    EvmConfig, EvmConfigSelector, EvmTypes, SpecId, VersionTables,
    evm::config::SelectorVersionTables,
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

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $vt:ident, $previous_vt:ident, $instr_fn:ty, $dispatch:tt] $($op:literal,)*) => {
        $(
            assign_instruction_table_entry!([$table, $vt, $previous_vt, $instr_fn, $dispatch] $op);
        )*
    };
}

macro_rules! assign_instruction_table_entry {
    ([$table:expr, $vt:ident, $previous_vt:ident, $instr_fn:ty, [$($dispatch:tt)*]] $op:literal) => {{
        let changed = super::instruction_changed($vt, $previous_vt, $op);
        if changed {
            $table[$op] = if $vt.is_unknown_opcode($op) {
                $($dispatch)* 0xFE, false, true> as $instr_fn
            } else if $vt.instruction($op).dynamic_gas {
                $($dispatch)* $op, true, false> as $instr_fn
            } else {
                $($dispatch)* $op, false, false> as $instr_fn
            };
        }
    }};
}

pub(super) const fn make_table<T, C, M>(
    previous: Option<&RawInstrTable<T>>,
    previous_version_tables: Option<&VersionTables<T>>,
) -> RawInstrTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
    M: InspectMode<T>,
{
    let mut table = match previous {
        Some(previous) => *previous,
        None => [tail_dispatch::<T, C, M, 0xFE, false, true> as super::imp::RawInstrFn<T>; 256],
    };
    let vt = C::VERSION_TABLES;
    for_each_opcode_value!([table, vt, previous_version_tables, super::imp::RawInstrFn<T>, [tail_dispatch::<T, C, M,]] assign_instruction_table_entries);
    table
}

pub(super) const fn make_selector_tables<T, F, M, const CUSTOM_SPEC_ID: u8>()
-> [RawInstrTable<T>; SpecId::COUNT]
where
    T: EvmTypes,
    F: EvmConfigSelector<T>,
    M: InspectMode<T>,
{
    macro_rules! make_tables {
        ([$($extra:tt)*] $($spec:ident $name:ident,)*) => {{
            make_tables!(@build [] [none]; $($spec $name,)*)
        }};
        (@build [$($tables:ident,)*] [$($previous_table:tt)*]; $spec:ident $name:ident, $($rest:ident $rest_name:ident,)*) => {{
            let spec = SpecId::$spec;
            let previous = spec.prev();
            let $name = make_table::<T, F::Config<{ SpecId::$spec as u8 }, CUSTOM_SPEC_ID>, M>(
                make_tables!(@previous_table [$($previous_table)*]),
                match previous {
                    Some(previous) => {
                        Some(SelectorVersionTables::<T, F, CUSTOM_SPEC_ID>::VERSION_TABLES[previous as usize])
                    }
                    None => None,
                },
            );
            make_tables!(@build [$($tables,)* $name,] [some $name]; $($rest $rest_name,)*)
        }};
        (@build [$($tables:ident,)*] [$($previous_table:tt)*];) => {
            [$($tables,)*]
        };
        (@previous_table [none]) => {
            None
        };
        (@previous_table [some $previous_table:ident]) => {
            Some(&$previous_table)
        };
    }

    crate::for_each_spec!([] make_tables)
}

extern_table! {
    fn tail_dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const OP: u8,
        const DYNAMIC_GAS: bool,
        const UNKNOWN: bool,
    >(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
        instructions: *const (),
    ) {
        if !UNKNOWN {
            unsafe { core::hint::assert_unchecked(pc.op() == OP) };
        }
        tail_return!(tail_dispatch_mono::<T, C, M, DYNAMIC_GAS, UNKNOWN>(
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
