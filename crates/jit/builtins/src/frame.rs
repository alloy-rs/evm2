//! Runtime operations for compiled frame instructions.

use super::*;

#[inline]
pub unsafe fn __revmc_builtin_approve<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 3],
) -> BuiltinResult {
    let rev![offset, len, scope] = sp;
    if ecx.host().is_assert_frame() {
        return Err(InstrStop::StateChangeDuringStaticCall.into());
    }
    let len = word_to_usize(len.to_u256())?;
    let offset = if len == 0 { 0 } else { word_to_usize(offset.to_u256())? };
    let output = output_memory_range(ecx, offset, len)?;
    let address = ecx.message().destination;
    let left = ecx.gas.reservoir();
    let charge = ecx
        .host()
        .approve_frame(address, scope.to_u256(), left)
        .map_err(|e| ecx.interpreter_mut().fail(e))?;
    ecx.gas.spend_state(charge)?;
    ecx.interpreter_mut().set_output(output);
    Err(InstrStop::Return.into())
}

#[inline]
pub unsafe fn __revmc_builtin_txparam<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    param: &mut EvmWord,
) -> BuiltinResult {
    let left = ecx.gas.reservoir();
    *param =
        ecx.host().frame_tx_param(param.to_u256(), left).ok_or(InstrStop::OpcodeNotFound)?.into();
    Ok(())
}

#[inline]
pub unsafe fn __revmc_builtin_framedataload<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![offset, index] = sp;
    let data = ecx.host().frame_data(index.to_u256()).ok_or(InstrStop::OpcodeNotFound)?;
    let offset = offset.to_u256().saturating_to::<usize>();
    *index = U256::from_be_bytes(read_word_padded(&data, offset).0).into();
    Ok(())
}

#[inline]
pub unsafe fn __revmc_builtin_frameparam<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![index, param] = sp;
    *param = ecx
        .host()
        .frame_param(index.to_u256(), param.to_u256())
        .ok_or(InstrStop::OpcodeNotFound)?
        .into();
    Ok(())
}

#[inline]
pub unsafe fn __revmc_builtin_sigparam<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![index, param] = sp;
    *param = ecx
        .host()
        .frame_signature_param(index.to_u256(), param.to_u256())
        .ok_or(InstrStop::OpcodeNotFound)?
        .into();
    Ok(())
}

#[inline]
pub unsafe fn __revmc_builtin_txtrace<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 2],
) -> BuiltinResult {
    let rev![param, index] = sp;
    *index = ecx
        .host()
        .txtrace(param.to_u256(), index.to_u256())
        .ok_or(InstrStop::OpcodeNotFound)?
        .into();
    Ok(())
}

#[inline]
pub unsafe fn __revmc_builtin_framedatacopy<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 4],
) -> BuiltinResult {
    let rev![memory, offset, len, index] = sp;
    let data = ecx.host().frame_data(index.to_u256()).ok_or(InstrStop::OpcodeNotFound)?;
    copy_frame_bytes(ecx, memory.to_u256(), offset.to_u256(), len.to_u256(), &data, false)
}

#[inline]
pub unsafe fn __revmc_builtin_sigdatacopy<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 4],
) -> BuiltinResult {
    let rev![memory, offset, len, index] = sp;
    let data =
        ecx.host().frame_signature_bytes(index.to_u256()).ok_or(InstrStop::OpcodeNotFound)?;
    copy_frame_bytes(ecx, memory.to_u256(), offset.to_u256(), len.to_u256(), &data, false)
}

#[inline]
pub unsafe fn __revmc_builtin_eventdatacopy<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    sp: &mut [EvmWord; 4],
) -> BuiltinResult {
    let rev![index, memory, offset, len] = sp;
    let data = ecx.host().event_data(index.to_u256()).ok_or(InstrStop::OpcodeNotFound)?;
    copy_frame_bytes(ecx, memory.to_u256(), offset.to_u256(), len.to_u256(), &data, true)
}

fn copy_frame_bytes<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    memory: U256,
    offset: U256,
    len: U256,
    data: &[u8],
    checked: bool,
) -> BuiltinResult {
    let len = word_to_usize(len)?;
    let offset = offset.saturating_to::<usize>();
    if checked && offset.checked_add(len).is_none_or(|end| end > data.len()) {
        return Err(InstrStop::OutOfOffset.into());
    }
    ecx.gas.spend(ecx.gas_params().copy_cost(len))?;
    if len != 0 {
        let memory = word_to_usize(memory)?;
        ensure_memory(ecx, memory, len)?;
        ecx.memory_mut().set_data(memory, offset, len, data);
    }
    Ok(())
}
