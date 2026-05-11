//! Custom opcode definitions.

use crate::config::CustomTypes;
use evm2::{
    interpreter::{Host, InstrStop, InterpreterState, Pc, StackMut, Word, private::Instruction},
    version::{GasId, GasParams},
};
use evm2_macros::instruction;

pub const CUSTOM_OPCODE: u8 = 0x0c;
pub const CUSTOM_OPCODE_GAS: u16 = 7;
pub const CUSTOM_OPCODE_DYNAMIC_GAS_ID: GasId = GasId::Custom0;
pub const CUSTOM_OPCODE_DYNAMIC_GAS: u32 = 3;
pub const L1_BLOCKNUMBER_OPCODE: u8 = 0x0d;
pub const L1_BLOCKNUMBER_GAS: u16 = 2;

pub const fn install_gas_params(gas_params: &mut GasParams) {
    gas_params.set(CUSTOM_OPCODE_DYNAMIC_GAS_ID, CUSTOM_OPCODE_DYNAMIC_GAS);
}

// Static gas comes from the instruction table; the dynamic part comes from the active version.
#[instruction(dynamic_gas)]
pub fn custom(cx: _) -> Result<out> {
    cx.gas.spend(cx.state.gas_params().get(CUSTOM_OPCODE_DYNAMIC_GAS_ID).into())?;
    *out = Word::from(0xdead_u64);
}

#[allow(non_camel_case_types)]
#[derive(Clone, Copy, derive_more::Debug)]
pub struct l1_blocknumber(core::marker::PhantomData<fn() -> CustomTypes>);

impl Instruction<CustomTypes> for l1_blocknumber {
    const DYNAMIC_GAS: bool = false;

    #[inline]
    fn execute(
        _pc: &mut Pc,
        mut stack: StackMut<'_>,
        state: &mut InterpreterState<'_, CustomTypes>,
    ) -> Result<(), InstrStop> {
        stack.push(Word::from(state.host().block_env().ext.l1_block_number))
    }
}
