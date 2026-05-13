use super::InspectMode;
use crate::{
    EvmConfig, EvmTypes,
    constants::STACK_LIMIT,
    interpreter::{InterpreterState, Pc, Stack, gas::RemainingGas},
};

/// Packed instruction return value.
pub(crate) type InstrFnRet = (Pc, PackedGasStackLen);

/// Packed instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> = extern_table!(
    fn(
        pc: Pc,
        stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet
);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypes>(
    instr: RawInstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    remaining_gas: &mut super::LoopState,
) -> (Pc, usize) {
    let (next_pc, gas_stack_len) = instr(pc, stack, *remaining_gas, state);
    let (next_remaining_gas, next_stack_len) = gas_stack_len.unpack();
    *remaining_gas = RemainingGas::new(next_remaining_gas);
    (next_pc, next_stack_len)
}

extern_table! {
    pub(in crate::interpreter::dispatch) fn dispatch<
        T: EvmTypes,
        C: EvmConfig<T>,
        M: InspectMode<T>,
        const OP: u8,
    >(
        pc: Pc,
        mut stack: Stack<'_>,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, T>,
    ) -> InstrFnRet {
        let opcode = OP;
        let (pc, remaining_gas) =
            super::dispatch_inner::<T, C, M, RemainingGas>(
                pc,
                stack.as_mut(),
                remaining_gas,
                state,
                opcode,
            );
        (
            pc,
            PackedGasStackLen::new(remaining_gas.get(), stack.len),
        )
    }
}

const STACK_LEN_BITS: u32 = 11;
const GAS_BITS: u32 = usize::BITS - STACK_LEN_BITS;
const GAS_MASK: usize = usize::MAX >> STACK_LEN_BITS;

const _: () = assert!(STACK_LIMIT <= (1 << STACK_LEN_BITS));

/// Dispatch remaining gas and stack length, packed into one word on 64-bit native targets.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct PackedGasStackLen(usize);

impl PackedGasStackLen {
    #[inline(always)]
    const fn new(remaining_gas: u64, stack_len: usize) -> Self {
        Self((stack_len << GAS_BITS) | (remaining_gas as usize & GAS_MASK))
    }

    #[inline(always)]
    const fn unpack(self) -> (u64, usize) {
        ((self.0 & GAS_MASK) as u64, self.0 >> GAS_BITS)
    }
}
