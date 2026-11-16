//! Bogota frame introspection, approval and assertion instructions.

use crate::{
    EvmTypesHost,
    interpreter::{Host, InstrStop, Result, Word, private::GasInstructionCx},
    utils::{WordExt, b256_to_word, read_word_padded},
};
use alloy_primitives::Bytes;
use evm2_macros::instruction;

#[instruction(dynamic_gas)]
pub fn approve(cx: _, [offset, len, scope]: [Word]) -> Result {
    if cx.state.host().is_assert_frame() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    let output = super::control::output_memory_range(cx.gas, cx.state, offset, len)?;
    let address = cx.state.message().destination;
    let left = cx.gas.reservoir();
    let charge =
        cx.state.host().approve_frame(address, *scope, left).map_err(|e| cx.state.fail(e))?;
    cx.gas.spend_state(charge)?;
    cx.state.set_output(output);
    Err(InstrStop::Return)
}

#[instruction(dynamic_gas)]
pub fn txparam(cx: _, [param]: [Word]) -> Result<out> {
    let left = cx.gas.reservoir();
    *out = cx.state.host().frame_tx_param(*param, left).ok_or(InstrStop::OpcodeNotFound)?;
}

#[instruction]
pub fn framedataload(cx: _, [offset, index]: [Word]) -> Result<out> {
    let data = cx.state.host().frame_data(*index).ok_or(InstrStop::OpcodeNotFound)?;
    *out = b256_to_word(read_word_padded(&data, offset.to_usize_saturated()));
}

/// Checks optional source bounds, charges copy gas, and copies frame or signature data.
fn copy_data<T: EvmTypesHost>(
    cx: GasInstructionCx<'_, '_, '_, T>,
    memory: &Word,
    offset: &Word,
    len: &Word,
    data: Bytes,
    checked: bool,
) -> Result {
    let len = len.to_usize()?;
    let data_offset = offset.to_usize_saturated();
    if checked && data_offset.checked_add(len).is_none_or(|end| end > data.len()) {
        return Err(InstrStop::OutOfOffset);
    }
    // SAFETY: Charging and copying data make no host calls.
    let gas_params = unsafe { cx.state.gas_params_detached() };
    cx.gas.spend(gas_params.copy_cost(len))?;
    super::env::copy_data(cx.gas, &mut cx.state.0.memory, gas_params, memory, offset, len, &data)
}

#[instruction(dynamic_gas)]
pub fn framedatacopy(cx: _, [memory, offset, len, index]: [Word]) -> Result {
    let data = cx.state.host().frame_data(*index).ok_or(InstrStop::OpcodeNotFound)?;
    copy_data(cx, memory, offset, len, data, false)
}

#[instruction]
pub fn frameparam(cx: _, [index, param]: [Word]) -> Result<out> {
    *out = cx.state.host().frame_param(*index, *param).ok_or(InstrStop::OpcodeNotFound)?;
}

#[instruction]
pub fn sigparam(cx: _, [index, param]: [Word]) -> Result<out> {
    *out =
        cx.state.host().frame_signature_param(*index, *param).ok_or(InstrStop::OpcodeNotFound)?;
}

#[instruction(dynamic_gas)]
pub fn sigdatacopy(cx: _, [memory, offset, len, index]: [Word]) -> Result {
    let data = cx.state.host().frame_signature_bytes(*index).ok_or(InstrStop::OpcodeNotFound)?;
    copy_data(cx, memory, offset, len, data, false)
}

#[instruction]
pub fn txtrace(cx: _, [param, index]: [Word]) -> Result<out> {
    *out = cx.state.host().txtrace(*param, *index).ok_or(InstrStop::OpcodeNotFound)?;
}

#[instruction(dynamic_gas)]
pub fn eventdatacopy(cx: _, [index, memory, offset, len]: [Word]) -> Result {
    let data = cx.state.host().event_data(*index).ok_or(InstrStop::OpcodeNotFound)?;
    copy_data(cx, memory, offset, len, data, true)
}
