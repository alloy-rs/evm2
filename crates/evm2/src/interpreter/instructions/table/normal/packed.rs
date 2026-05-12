use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    constants::STACK_LIMIT,
    interpreter::{InterpreterState, Pc, Result, Stack, gas::RemainingGas},
};
use core::hint::cold_path;

/// Packed normal instruction return value.
pub(crate) type InstrFnRet = (Pc, PackedGasStackLen);

/// Packed normal instruction function pointer.
pub(super) type RawInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet
);

extern_table! {
    pub(super) fn dispatch<
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
                super::super::inc_pc(&mut pc, op);
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
        return dispatch_return(
            Pc::new(core::ptr::null()),
            initial_remaining_gas.get().wrapping_sub(remaining_gas.get()),
            stack_len,
        );
    }
    dispatch_return(pc, initial_remaining_gas.get().wrapping_sub(remaining_gas.get()), stack.len)
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

#[inline(always)]
const fn dispatch_return(pc: Pc, gas_spent: u64, stack_len: usize) -> InstrFnRet {
    (pc, PackedGasStackLen::new(gas_spent, stack_len))
}

#[inline(always)]
pub(crate) const fn unpack_ret(ret: InstrFnRet) -> (Pc, u64, usize) {
    let (pc, gas_stack_len) = ret;
    let (gas_spent, stack_len) = gas_stack_len.unpack();
    (pc, gas_spent, stack_len)
}

/// Normal dispatch gas spent and stack length, packed into one word on 64-bit native targets.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct PackedGasStackLen(usize);

impl PackedGasStackLen {
    #[inline(always)]
    const fn new(gas_spent: u64, stack_len: usize) -> Self {
        debug_assert!(stack_len <= STACK_LIMIT && gas_spent as usize <= GAS_MASK);
        Self((stack_len << GAS_BITS) | (gas_spent as usize & GAS_MASK))
    }

    #[inline(always)]
    const fn unpack(self) -> (u64, usize) {
        ((self.0 & GAS_MASK) as u64, self.0 >> GAS_BITS)
    }
}
