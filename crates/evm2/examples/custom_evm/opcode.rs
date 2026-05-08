//! Custom opcode definition.

use evm2::{
    interpreter::Word,
    version::{GasId, GasParams},
};
use evm2_macros::instruction;

pub(crate) const CUSTOM_OPCODE: u8 = 0x0c;
pub(crate) const CUSTOM_OPCODE_GAS: u16 = 7;
pub(crate) const CUSTOM_OPCODE_DYNAMIC_GAS_ID: GasId = GasId::Custom0;
pub(crate) const CUSTOM_OPCODE_DYNAMIC_GAS: u32 = 3;

pub(crate) const fn install_gas_params(gas_params: &mut GasParams) {
    gas_params.set(CUSTOM_OPCODE_DYNAMIC_GAS_ID, CUSTOM_OPCODE_DYNAMIC_GAS);
}

// Static gas comes from the instruction table; the dynamic part comes from the active version.
#[instruction(dynamic_gas)]
pub(crate) fn custom(cx: _) -> Result<out> {
    cx.gas.spend(cx.state.gas_params().get(CUSTOM_OPCODE_DYNAMIC_GAS_ID).into())?;
    *out = Word::from(0xdead_u64);
}
