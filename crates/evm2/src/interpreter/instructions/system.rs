//! System opcode implementations.

use crate::{
    EvmFeatures, EvmTypes,
    bytecode::Bytecode,
    interpreter::{
        Gas, Host, InstrStop, InterpreterState, Message, MessageKind, Result, StackMut, Word,
        memory::resize_memory,
    },
    utils::{word_to_address, word_to_usize},
    version::GasId,
};
use alloy_primitives::{Address, B256, Bytes};
use core::{cmp::min, ops::Range};
use evm2_macros::instruction;

#[inline]
const fn require_non_staticcall<T: EvmTypes>(state: &InterpreterState<'_, T>) -> Result {
    if state.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    Ok(())
}

#[inline]
const fn should_charge_new_account_gas(
    eip161: bool,
    transfers_value: bool,
    target_is_empty_for_new_account_gas: bool,
) -> bool {
    target_is_empty_for_new_account_gas && (!eip161 || transfers_value)
}

fn resize_memory_range<T: EvmTypes>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, T>,
    offset: Word,
    len: Word,
) -> Result<Range<usize>> {
    let len = word_to_usize(len)?;
    let offset = if len != 0 {
        let offset = word_to_usize(offset)?;
        resize_memory(gas, state.memory(), offset, len)?;
        offset
    } else {
        usize::MAX
    };
    Ok(offset..offset + len)
}

fn get_memory_input_and_out_ranges<T: EvmTypes>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, T>,
    input_offset: Word,
    input_len: Word,
    return_offset: Word,
    return_len: Word,
) -> Result<(Range<usize>, Range<usize>)> {
    let input = resize_memory_range(gas, state, input_offset, input_len)?;
    let output = resize_memory_range(gas, state, return_offset, return_len)?;
    Ok((input, output))
}

fn memory_range_bytes<T: EvmTypes>(
    state: &mut InterpreterState<'_, T>,
    range: Range<usize>,
) -> Result<Bytes> {
    if range.is_empty() {
        return Ok(Bytes::new());
    }
    Ok(Bytes::copy_from_slice(state.memory().slice(range.start, range.len())))
}

fn load_acc_and_calc_gas<T: EvmTypes>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, T>,
    to: Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytecode, Address, bool)> {
    if transfers_value {
        gas.spend(state.gas_params().get(GasId::TransferValueCost).into())?;
    }

    let additional_cold_cost = state.gas_params().cold_account_additional_cost();
    let remaining_gas = gas.remaining();
    let skip_cold_load = remaining_gas < additional_cold_cost;
    let account = state.host().load_account(&to, true, skip_cold_load)?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    let mut code = account.code;
    let mut code_address = to;
    if state.feature(EvmFeatures::EIP7702)
        && let Some(delegated_address) = code.eip7702_address()
    {
        cost += u64::from(state.gas_params().get(GasId::WarmStorageReadCost));
        if cost > remaining_gas {
            return Err(InstrStop::OutOfGas);
        }
        let skip_cold_load = remaining_gas < cost.saturating_add(additional_cold_cost);
        let delegated_account =
            state.host().load_account(&delegated_address, true, skip_cold_load)?;
        if delegated_account.is_cold {
            cost += additional_cold_cost;
        }
        code = delegated_account.code;
        code_address = delegated_address;
    }
    let features = state.version().features;
    if create_empty_account
        && should_charge_new_account_gas(
            features.contains(EvmFeatures::EIP161),
            transfers_value,
            state.host().target_is_empty_for_new_account_gas(&to, features)?,
        )
    {
        cost += u64::from(state.gas_params().get(GasId::NewAccountCost));
    }
    gas.spend(cost)?;

    let mut gas_limit = if state.feature(EvmFeatures::EIP150) {
        min(state.gas_params().call_stipend_reduction(gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    gas.spend(gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(state.gas_params().get(GasId::CallStipend).into());
    }

    let disable_precompiles = code_address != to;
    Ok((gas_limit, code, code_address, disable_precompiles))
}

#[inline(never)]
fn prepare_call<T: EvmTypes>(
    mut stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, T>,
    kind: MessageKind,
    message: &mut Message<T>,
    code: &mut Bytecode,
    return_memory_range: &mut Range<usize>,
) -> Result {
    let has_value = match kind {
        MessageKind::Call | MessageKind::CallCode => true,
        MessageKind::DelegateCall | MessageKind::StaticCall => false,
        _ => unreachable!("invalid call message kind"),
    };
    let [local_gas_limit, to] = stack.popn::<2>()?;
    let value = if has_value { stack.pop()? } else { Word::ZERO };
    let [input_offset, input_len, return_offset, return_len] = stack.popn::<4>()?;
    let to = word_to_address(to);
    let has_transfer = !value.is_zero();
    if state.is_static() && kind == MessageKind::Call && has_transfer {
        return Err(InstrStop::CallNotAllowedInsideStatic);
    }

    let local_gas_limit = u64::try_from(local_gas_limit).unwrap_or(u64::MAX);
    let (input_range, prepared_return_memory_range) = get_memory_input_and_out_ranges(
        gas,
        state,
        input_offset,
        input_len,
        return_offset,
        return_len,
    )?;
    let (gas_limit, loaded_code, resolved_code_address, disable_precompiles) =
        load_acc_and_calc_gas(
            gas,
            state,
            to,
            has_transfer,
            kind == MessageKind::Call,
            local_gas_limit,
        )?;
    let input = memory_range_bytes(state, input_range)?;

    let current = state.message();
    let (destination, caller, call_value, code_address) = match kind {
        MessageKind::Call => (to, current.destination, value, resolved_code_address),
        MessageKind::CallCode => {
            (current.destination, current.destination, value, resolved_code_address)
        }
        MessageKind::DelegateCall => {
            (current.destination, current.caller, current.value, resolved_code_address)
        }
        MessageKind::StaticCall => (to, current.destination, Word::ZERO, resolved_code_address),
        _ => unreachable!("invalid call message kind"),
    };
    *message = Message {
        kind,
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination,
        caller,
        input,
        value: call_value,
        code_address,
        disable_precompiles,
        caller_is_static: state.is_static(),
        salt: B256::ZERO,
        ext: T::MessageExt::default(),
        _non_exhaustive: (),
    };
    *code = loaded_code;
    *return_memory_range = prepared_return_memory_range;

    Ok(())
}

#[inline(never)]
fn call_inner<T: EvmTypes>(
    mut stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, T>,
    kind: MessageKind,
) -> Result {
    let mut message = Message::<T>::default();
    let mut code = Bytecode::default();
    let mut return_memory_range = 0..0;
    prepare_call(
        stack.reborrow(),
        gas,
        state,
        kind,
        &mut message,
        &mut code,
        &mut return_memory_range,
    )?;

    let tx_env = state.tx();
    let mut result = state.host().execute_message(tx_env, code, &mut message);
    gas.erase_cost(result.gas_returned_to_parent());
    gas.record_refund(result.refund_propagated_to_parent());
    let copy_len = min(return_memory_range.len(), result.output.len());
    unsafe {
        let output = result.output.get_unchecked(..copy_len);
        state.memory().set_unchecked(return_memory_range.start, output);
    }
    state.swap_return_data(&mut result.output);
    stack.push(Word::from(result.stop.is_success()))
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn call(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::Call)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn callcode(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::CallCode)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn delegatecall(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::DelegateCall)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn staticcall(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::StaticCall)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn create<const IS_CREATE2: bool>(cx: _) -> Result {
    create_inner(stack, cx.gas, cx.state, IS_CREATE2)
}

#[inline(never)]
fn create_inner<T: EvmTypes>(
    mut stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, T>,
    is_create2: bool,
) -> Result {
    require_non_staticcall(state)?;

    let [value, offset, len] = stack.popn::<3>()?;
    let salt = if is_create2 { Some(stack.pop()?) } else { None };

    let len = word_to_usize(len)?;
    if state.feature(EvmFeatures::EIP3860) {
        if len > state.version().max_initcode_size {
            return Err(InstrStop::CreateInitCodeSizeLimit);
        }
        gas.spend(state.gas_params().initcode_cost(len))?;
    }
    let code_range = resize_memory_range(gas, state, offset, Word::from(len))?;
    let input = memory_range_bytes(state, code_range)?;
    let create_cost = if is_create2 {
        state.gas_params().create2_cost(len)
    } else {
        state.gas_params().get(GasId::Create).into()
    };
    gas.spend(create_cost)?;
    let gas_limit = if state.feature(EvmFeatures::EIP150) {
        state.gas_params().call_stipend_reduction(gas.remaining())
    } else {
        gas.remaining()
    };
    gas.spend(gas_limit)?;

    let current = state.message();
    let mut message = Message {
        kind: if is_create2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input,
        value,
        code_address: current.destination,
        disable_precompiles: false,
        // CREATE is rejected in a static context (see `require_non_staticcall`).
        caller_is_static: false,
        salt: salt.map(|salt| B256::from(salt.to_be_bytes())).unwrap_or_default(),
        ext: T::MessageExt::default(),
        _non_exhaustive: (),
    };
    let bytecode = crate::bytecode::Bytecode::new_legacy(message.input.clone());
    let tx_env = state.tx();
    let result = state.host().execute_message(tx_env, bytecode, &mut message);
    gas.erase_cost(result.gas_returned_to_parent());
    gas.record_refund(result.refund_propagated_to_parent());
    // EIP-211 exposes CREATE failure data only for REVERT; other failures clear returndata.
    if result.stop == InstrStop::Revert {
        state.set_return_data(result.output);
    } else {
        state.set_return_data(Bytes::new());
    }
    let address = result
        .created_address
        .filter(|_| result.stop.is_success())
        .map(|address| Word::from_be_slice(address.as_slice()))
        .unwrap_or_default();
    stack.push(address)
}

#[instruction(dynamic_gas)]
pub(crate) fn selfdestruct(cx: _, [target]: [Word]) -> Result {
    require_non_staticcall(cx.state)?;
    let target = word_to_address(*target);
    let cold_load_gas = cx.state.gas_params().selfdestruct_cold_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let destination = &cx.state.message().destination;
    let res = cx.state.host().selfdestruct(destination, &target, skip_cold_load)?;
    cx.state.inspect_selfdestruct(destination, &target, &res.value);
    let should_charge_topup = should_charge_new_account_gas(
        cx.state.feature(EvmFeatures::EIP161),
        res.had_value,
        res.target_is_empty,
    );
    cx.gas.spend(cx.state.gas_params().selfdestruct_cost(should_charge_topup, res.is_cold))?;
    if !res.previously_destroyed {
        cx.gas.record_refund(cx.state.gas_params().get(GasId::SelfdestructRefund) as i64);
    }
    Err(InstrStop::SelfDestruct)
}

#[cfg(test)]
mod tests {
    use crate::{
        SpecId,
        constants::{CALL_DEPTH_LIMIT, MAX_INITCODE_SIZE},
        interpreter::{
            InstrStop, Message, MessageKind, MessageResult, Word,
            instructions::tests::{RunConfig, TestHost, push, run},
            op,
        },
        utils::address_to_word,
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, Bytes};
    use core::assert_matches;

    fn push_all<const N: usize>(code: &mut Vec<u8>, values: [Word; N]) {
        for value in values {
            push(code, value);
        }
    }

    #[test]
    fn call_opcode() {
        let target = Address::from([0x22; 20]);
        let caller = Address::from([0x11; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                address_to_word(&target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).message(Message {
            destination: caller,
            gas_limit: 10_000,
            ..Default::default()
        }));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(1)]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert_eq!(host.calls[0].destination, target);
        assert_eq!(host.calls[0].caller, caller);
    }

    #[test]
    fn call_propagates_static_flag() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::ZERO,
                address_to_word(&target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).staticcall());
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert!(host.call_static_flags[0]);
    }

    #[test]
    fn call_too_deep_charges_dynamic_gas() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost { is_cold: true, is_empty: true, ..Default::default() };
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::from(1),
                address_to_word(&target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 15_679);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_with_value_too_deep_credits_stipend() {
        let mut host = TestHost::default();
        let code = [
            op::PUSH1,
            0xff,
            op::PUSH1,
            0,
            op::PUSH1,
            0xff,
            op::PUSH1,
            0,
            op::PUSH1,
            1,
            op::PUSH1,
            0xaa,
            op::PUSH2,
            0x80,
            0,
            op::CALL,
            op::POP,
        ];

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.gas_remaining(), 42_553);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_too_deep_charges_pre_spurious_empty_account() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost { exists: false, is_empty: true, ..Default::default() };
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                address_to_word(&target),
                Word::ZERO,
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 24_279);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_too_deep_skips_pre_spurious_touched_empty_account_cost() {
        let target = Address::from([0x22; 20]);
        let mut host =
            TestHost { exists: false, is_empty: true, is_touched: true, ..Default::default() };
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                address_to_word(&target),
                Word::ZERO,
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 49_279);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn callcode_propagates_static_flag_and_allows_apparent_value() {
        let code_address = Address::from([0x33; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(7),
                address_to_word(&code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).staticcall().gas_limit(20_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::CallCode);
        assert_eq!(host.calls[0].value, Word::from(7));
        assert!(host.call_static_flags[0]);
    }

    #[test]
    fn callcode_opcode() {
        let code_address = Address::from([0x33; 20]);
        let destination = Address::from([0x11; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(7),
                address_to_word(&code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .message(Message { destination, ..Default::default() })
            .gas_limit(20_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(1)]);
        assert_eq!(host.calls[0].kind, MessageKind::CallCode);
        assert_eq!(host.calls[0].destination, destination);
        assert_eq!(host.calls[0].code_address, code_address);
        assert_eq!(host.calls[0].value, Word::from(7));
    }

    #[test]
    fn delegatecall_opcode() {
        let code_address = Address::from([0x44; 20]);
        let caller = Address::from([0x55; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                address_to_word(&code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::DELEGATECALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).message(Message {
            caller,
            value: Word::from(9),
            gas_limit: 10_000,
            ..Default::default()
        }));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(1)]);
        assert_eq!(host.calls[0].kind, MessageKind::DelegateCall);
        assert_eq!(host.calls[0].caller, caller);
        assert_eq!(host.calls[0].value, Word::from(9));
        assert_eq!(host.calls[0].code_address, code_address);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::DELEGATECALL,
        ])
        .spec(SpecId::FRONTIER));
        assert_matches!(interp.err, InstrStop::InvalidOpcode);
    }

    #[test]
    fn staticcall_opcode() {
        let target = Address::from([0x66; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::from(0),
                Word::from(0),
                Word::from(0),
                Word::from(0),
                address_to_word(&target),
                Word::from(1000),
            ],
        );
        code.extend([op::STATICCALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(1)]);
        assert_eq!(host.calls[0].kind, MessageKind::StaticCall);
        assert!(host.call_static_flags[0]);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::STATICCALL,
        ])
        .spec(SpecId::HOMESTEAD));
        assert_matches!(interp.err, InstrStop::InvalidOpcode);
    }

    #[test]
    fn create_opcode() {
        let created = Address::from([0x77; 20]);
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(0), Word::from(0), Word::from(0)]);
        code.extend([op::CREATE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&created)]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Create);
    }

    #[test]
    fn create_clears_return_data_on_success() {
        let created = Address::from([0x77; 20]);
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                output: Bytes::from_static(&[0xaa, 0xbb, 0xcc]),
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(0), Word::from(0), Word::from(0)]);
        code.extend([op::CREATE, op::RETURNDATASIZE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&created), Word::ZERO]);
    }

    #[test]
    fn create_sets_return_data_on_revert() {
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Revert,
                output: Bytes::from_static(&[0xaa, 0xbb]),
                ..MessageResult::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(0), Word::from(0), Word::from(0)]);
        code.extend([op::CREATE, op::RETURNDATASIZE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO, Word::from(2)]);
    }

    #[test]
    fn create_too_deep_charges_create_gas() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 17_991);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_initcode_size_limit_halts_after_shanghai() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(MAX_INITCODE_SIZE + 1), Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let interp =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::SHANGHAI).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::CreateInitCodeSizeLimit);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create2_opcode() {
        let created = Address::from([0x88; 20]);
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(0), Word::from(0), Word::from(0), Word::from(0)]);
        code.extend([op::CREATE2, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&created)]);
        assert_eq!(host.calls[0].kind, MessageKind::Create2);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::CREATE2,
        ])
        .spec(SpecId::BYZANTIUM));
        assert_matches!(interp.err, InstrStop::InvalidOpcode);
    }

    #[test]
    fn selfdestruct_opcode() {
        let contract = Address::from([0x11; 20]);
        let target = Address::from([0x99; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let interp = run(RunConfig::new(code).host(&mut host).message(Message {
            destination: contract,
            gas_limit: 10_000,
            ..Default::default()
        }));
        assert_matches!(interp.err, InstrStop::SelfDestruct);
        assert_eq!(host.selfdestructs, [(contract, target, false)]);
    }
}
