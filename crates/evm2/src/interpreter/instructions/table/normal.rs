use crate::{
    EvmConfig, EvmTypes,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::Gas},
};
use core::hint::cold_path;

/// Normal instruction return value.
pub(crate) type InstructionFnRet = (*const u8, usize);

/// Normal instruction function pointer.
pub(super) type RawInstructionFn<T> = extern_table!(
    fn(pc: Pc, stack: Stack<'_>, state: &mut InterpreterState<'_, T>) -> InstructionFnRet
);

/// Normal instruction dispatch table.
pub(super) type RawInstructionTable<T> = [RawInstructionFn<T>; 256];

macro_rules! assign_instruction_table_entries {
    ([$table:expr, $evm_types:ty, $config:ty, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            $table[$op] = $dispatch::<$evm_types, $config, $op> as $instr_fn;
        )*
    };
}

pub(crate) const fn make_instruction_table<T, C>() -> RawInstructionTable<T>
where
    T: EvmTypes,
    C: EvmConfig<T>,
{
    let mut table = [dispatch::<T, C, 0> as super::InstructionFn<T>; 256];
    for_each_opcode_value!([table, T, C, dispatch, super::InstructionFn<T>] assign_instruction_table_entries);

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

extern_table! {
    fn dispatch<T: EvmTypes, C: EvmConfig<T>, const OP: u8>(
        pc: Pc,
        stack: Stack<'_>,
        state: &mut InterpreterState<'_, T>,
    ) -> InstructionFnRet {
        dispatch_mono::<T, C>(OP, pc, stack, state)
    }
}

#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>>(
    op: u8,
    mut pc: Pc,
    mut stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
) -> InstructionFnRet {
    let instr = C::VERSION_TABLES.instruction(op).instr;
    let r;
    match pre_step::<T, C>(state.gas_mut(), op) {
        Ok(()) => {
            r = instr(&mut pc, stack.as_mut(), state);
            super::inc_pc(&mut pc, op);
        }
        Err(e) => r = Err(e),
    }
    if r.is_err() {
        cold_path();
        state.set_result(r);
        return (core::ptr::null(), stack.len);
    }
    (pc.as_ptr(), stack.len)
}

#[inline]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(gas: &mut Gas, op: u8) -> Result {
    gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}
