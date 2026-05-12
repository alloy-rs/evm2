use super::InspectMode;
use crate::{
    EvmConfig, EvmConfigSelector, EvmTypes, SpecId, VersionTables,
    constants::STACK_LIMIT,
    evm::config::SelectorVersionTables,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::RemainingGas},
};
use core::hint::cold_path;

/// Normal instruction return value.
pub(crate) type InstrFnRet = (Pc, PackedGasStackLen);

/// Normal instruction function pointer.
pub(super) type RawInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet
);

/// Normal instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $evm_types:ty, $config:ty, $mode:ty, $vt:ident, $previous_vt:ident, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            if super::instruction_changed($vt, $previous_vt, $op) {
                let instruction = <$config as EvmConfig<$evm_types>>::VERSION_TABLES.instruction($op);
                $table[$op] = if instruction.dynamic_gas {
                    $dispatch::<$evm_types, $config, $mode, $op, true> as $instr_fn
                } else {
                    $dispatch::<$evm_types, $config, $mode, $op, false> as $instr_fn
                };
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
        None => [dispatch::<T, C, M, 0, true> as super::InstrFn<T>; 256],
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

pub(crate) const fn make_selector_tables<T, F, M, const CUSTOM_SPEC_ID: u8>()
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
    fn dispatch<
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
    ) -> InstrFnRet {
        dispatch_mono::<T, C, M, DYNAMIC_GAS>(pc, stack, remaining_gas, state, OP)
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, M: InspectMode<T>, const DYNAMIC_GAS: bool>(
    mut pc: Pc,
    mut stack: Stack<'_>,
    mut remaining_gas: RemainingGas,
    state: &mut InterpreterState<'_, T>,
    op: u8,
) -> InstrFnRet {
    let initial_remaining_gas = remaining_gas;
    let instr = C::VERSION_TABLES.instruction(op).instr;
    if M::INSPECT {
        M::step(state, pc, stack.len);
    }
    let r;
    match pre_step::<T, C>(&mut remaining_gas, op) {
        Ok(()) => {
            if M::INSPECT || DYNAMIC_GAS {
                state.gas_mut().set_remaining(remaining_gas.get());
            }
            r = instr(&mut pc, stack.as_mut(), state);
            if DYNAMIC_GAS {
                remaining_gas.set(state.gas_mut().remaining());
            }
            if !M::INSPECT || r.is_ok() {
                super::inc_pc(&mut pc, op);
            }
        }
        Err(e) => {
            if M::INSPECT {
                state.gas_mut().set_remaining(remaining_gas.get());
            }
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
        let stack_len = if stack.len <= STACK_LIMIT { stack.len } else { 0 };
        return (
            Pc::new(core::ptr::null()),
            PackedGasStackLen::pack(
                initial_remaining_gas.get().wrapping_sub(remaining_gas.get()),
                stack_len,
            ),
        );
    }
    (
        pc,
        PackedGasStackLen::pack(
            initial_remaining_gas.get().wrapping_sub(remaining_gas.get()),
            stack.len,
        ),
    )
}

#[inline(always)]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
    remaining_gas: &mut RemainingGas,
    op: u8,
) -> Result {
    remaining_gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}

const STACK_LEN_BITS: u32 = 11;
const GAS_BITS: u32 = usize::BITS - STACK_LEN_BITS;
const GAS_MASK: usize = usize::MAX >> STACK_LEN_BITS;

const _: () = assert!(STACK_LIMIT <= (1 << STACK_LEN_BITS));

/// Packed normal dispatch gas spent and stack length.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct PackedGasStackLen(usize);

impl PackedGasStackLen {
    #[inline(always)]
    pub(crate) const fn pack(gas_spent: u64, stack_len: usize) -> Self {
        debug_assert!(stack_len <= STACK_LIMIT);
        Self((stack_len << GAS_BITS) | (gas_spent as usize & GAS_MASK))
    }

    #[inline(always)]
    pub(crate) const fn unpack(self) -> (u64, usize) {
        ((self.0 & GAS_MASK) as u64, self.0 >> GAS_BITS)
    }
}
