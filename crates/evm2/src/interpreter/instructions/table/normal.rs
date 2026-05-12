use super::InspectMode;
use crate::{
    EvmConfig, EvmConfigSelector, EvmTypes, SpecId, VersionTables,
    evm::config::SelectorVersionTables,
};

cfg_if::cfg_if! {
    if #[cfg(dispatch_single_return)] {
        use super::normal_single_return as imp;
    } else if #[cfg(dispatch_packed)] {
        use super::normal_packed as imp;
    } else {
        use super::normal_unpacked as imp;
    }
}

use imp::dispatch;

/// Normal instruction function pointer.
pub(super) type RawInstrFn<T> = imp::RawInstrFn<T>;

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
