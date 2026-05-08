use crate::{
    EvmConfig, EvmTypes,
    constants::STACK_LIMIT,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::RemainingGas},
};
use core::hint::cold_path;

/// Normal instruction return value.
pub(crate) type InstrFnRet = (PackedPcStackLen, RemainingGas);

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
    let mut table = [dispatch::<T, C, 0, true> as super::InstrFn<T>; 256];
    for_each_opcode_value!([table, T, C, dispatch, super::InstrFn<T>] assign_instruction_table_entries);

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
    fn dispatch<T: EvmTypes, C: EvmConfig<T>, const OP: u8, const DYNAMIC_GAS: bool>(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        dispatch_mono::<T, C, DYNAMIC_GAS>(OP, pc, stack, remaining_gas, state)
    }
}

#[inline(always)]
fn dispatch_mono<T: EvmTypes, C: EvmConfig<T>, const DYNAMIC_GAS: bool>(
    op: u8,
    mut pc: Pc,
    mut stack: Stack<'_>,
    mut remaining_gas: RemainingGas,
    state: &mut InterpreterState<'_, T>,
) -> InstrFnRet {
    let instr = C::VERSION_TABLES.instruction(op).instr;
    let r;
    match pre_step::<T, C>(&mut remaining_gas, op) {
        Ok(()) => {
            if DYNAMIC_GAS {
                state.gas_mut().set_remaining(remaining_gas.get());
            }
            r = instr(&mut pc, stack.as_mut(), state);
            if DYNAMIC_GAS {
                remaining_gas.set(state.gas_mut().remaining());
            }
            super::inc_pc(&mut pc, op);
        }
        Err(e) => r = Err(e),
    }
    if r.is_err() {
        cold_path();
        state.set_result(r);
        return (PackedPcStackLen::pack(core::ptr::null(), stack.len), remaining_gas);
    }
    (PackedPcStackLen::pack(pc.as_ptr(), stack.len), remaining_gas)
}

#[inline]
const fn pre_step<T: EvmTypes, C: EvmConfig<T>>(
    remaining_gas: &mut RemainingGas,
    op: u8,
) -> Result {
    remaining_gas.spend(C::VERSION_TABLES.static_gas(op) as _)
}

const STACK_LEN_BITS: u32 = 11;
const PC_BITS: u32 = usize::BITS - STACK_LEN_BITS;
const PC_MASK: usize = usize::MAX >> STACK_LEN_BITS;

const _: () = assert!(STACK_LIMIT <= (1 << STACK_LEN_BITS));

/// Packed normal dispatch pc and stack length.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct PackedPcStackLen(usize);

impl PackedPcStackLen {
    #[inline(always)]
    pub(crate) fn pack(pc: *const u8, stack_len: usize) -> Self {
        Self((stack_len << PC_BITS) | pc as usize)
    }

    #[inline(always)]
    pub(crate) const fn unpack(self) -> (*const u8, usize) {
        ((self.0 & PC_MASK) as *const u8, self.0 >> PC_BITS)
    }
}
