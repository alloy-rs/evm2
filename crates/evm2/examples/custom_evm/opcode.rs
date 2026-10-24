//! Custom opcode definitions.

use crate::config::CustomTypes;
use evm2::{
    interpreter::{Host, Word, instructions::sstore_with},
    version::{GasId, GasParams},
};
use evm2_macros::instruction;

pub const CUSTOM_OPCODE: u8 = 0x0c;
pub const CUSTOM_OPCODE_GAS: u16 = 7;
pub const CUSTOM_OPCODE_DYNAMIC_GAS_ID: GasId = GasId::Custom0;
pub const CUSTOM_OPCODE_DYNAMIC_GAS: u32 = 3;
pub const SSTORE_SURCHARGE: u64 = 1_000;
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

#[instruction(EvmTypes = CustomTypes)]
pub fn l1_blocknumber(cx: _) -> out {
    *out = Word::from(cx.state.host().block_env().ext.l1_block_number);
}

// A custom `SSTORE` that reuses the standard instruction and charges a surcharge for every write
// that changes the slot. The hook runs after the host write and before the standard dynamic gas.
#[instruction(EvmTypes = CustomTypes, dynamic_gas)]
pub fn custom_sstore(cx: _, [key, value]: [Word]) -> Result {
    sstore_with(cx.gas, cx.state, key, value, |gas, _state, _address, sstore| {
        if sstore.is_noop() { Ok(()) } else { gas.spend(SSTORE_SURCHARGE) }
    })
}
