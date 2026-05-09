use super::InspectMode;
use crate::{
    EvmConfig, EvmConfigSelector, EvmTypes, SpecId, VersionTables,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas},
};
use core::hint::cold_path;

/// Normal instruction return value.
pub(crate) type InstrFnRet = (*const u8, usize);

/// Normal instruction function pointer.
pub(super) type RawInstrFn<T> =
    extern_table!(fn(pc: Pc, stack: Stack<'_>, state: &mut InterpreterState<'_, T>) -> InstrFnRet);

/// Normal instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $evm_types:ty, $config:ty, $mode:ty, $vt:ident, $previous_vt:ident, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            if super::instruction_changed($vt, $previous_vt, $op) {
                $table[$op] = $dispatch::<$evm_types, $config, $mode, $op> as $instr_fn;
            }
        )*
    };
}

pub(crate) const fn make_table<T, C, M>(
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
        None => [dispatch::<T, C, M, 0> as super::InstrFn<T>; 256],
    };
    let vt = C::VERSION_TABLES;
    for_each_opcode_value!([table, T, C, M, vt, previous_version_tables, dispatch, super::InstrFn<T>] assign_instruction_table_entries);

    // Make all unknown entries point to the same dispatch function.
    let mut i = 0;
    let mut unknown_idx = None;
    while i < 256 {
        if C::VERSION_TABLES.is_unknown_opcode(i as u8) {
            if unknown_idx.is_none() {
                unknown_idx = Some(i);
            }
            table[i] = table[unknown_idx.unwrap()];
        }
        i += 1;
    }

    table
}

pub(crate) const fn make_selector_tables<T, F, M>() -> [RawInstrTable<T>; SpecId::COUNT]
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
            let $name = make_table::<T, F::Config<{ SpecId::$spec as u8 }>, M>(
                make_tables!(@previous_table [$($previous_table)*]),
                match previous {
                    Some(previous) => Some(F::VERSION_TABLES[previous as usize]),
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
    fn dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const OP: u8,
    >(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        dispatch_mono::<T, C, M>(pc, stack, state, OP)
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    op: u8,
) -> InstrFnRet {
    let instr = C::VERSION_TABLES.instruction(op).instr;
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match pre_step::<T, C>(state.gas_mut(), op) {
        Ok(()) => {
            r = instr(&mut pc, stack.as_mut(), state);
            if !M::INSPECT || r.is_ok() {
                super::inc_pc(&mut pc, op);
            }
        }
        Err(e) => r = Err(e),
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
        return (core::ptr::null(), stack.len);
    }
    (pc.as_ptr(), stack.len)
}

#[inline(always)]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(gas: &mut Gas, op: u8) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}
