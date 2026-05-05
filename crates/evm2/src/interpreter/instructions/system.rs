//! System opcode implementations.

use crate::{
    EvmTypes, SpecId,
    interpreter::{
        GasInstructionCx, Host, InstrStop, Message, MessageKind, Result, StackMut, State, Word,
        memory::resize_memory,
    },
    utils::{word_to_address, word_to_usize},
    version::GasId,
};
use alloy_primitives::{Address, B256, Bytes};
use core::{cmp::min, ops::Range};
use evm2_macros::instruction;

#[inline]
fn require_non_staticcall<T: EvmTypes>(state: &State<'_, T>) -> Result {
    if state.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    Ok(())
}

#[inline]
const fn success(stop: InstrStop) -> bool {
    matches!(stop, InstrStop::Stop | InstrStop::Return | InstrStop::SelfDestruct)
}

fn resize_memory_range<T: EvmTypes>(
    cx: &mut GasInstructionCx<'_, '_, T>,
    offset: Word,
    len: Word,
) -> Result<Range<usize>> {
    let len = word_to_usize(len)?;
    let offset = if len != 0 {
        let offset = word_to_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory(), offset, len)?;
        offset
    } else {
        usize::MAX
    };
    Ok(offset..offset + len)
}

fn get_memory_input_and_out_ranges<T: EvmTypes>(
    cx: &mut GasInstructionCx<'_, '_, T>,
    input_offset: Word,
    input_len: Word,
    return_offset: Word,
    return_len: Word,
) -> Result<(Range<usize>, Range<usize>)> {
    let input = resize_memory_range(cx, input_offset, input_len)?;
    let output = resize_memory_range(cx, return_offset, return_len)?;
    Ok((input, output))
}

fn memory_range_bytes<T: EvmTypes>(
    cx: &mut GasInstructionCx<'_, '_, T>,
    range: Range<usize>,
) -> Result<Bytes> {
    if range.is_empty() {
        return Ok(Bytes::new());
    }
    Ok(Bytes::copy_from_slice(cx.state.memory().slice(range.start, range.len())))
}

fn load_acc_and_calc_gas<T: EvmTypes>(
    cx: &mut GasInstructionCx<'_, '_, T>,
    to: Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytes)> {
    if transfers_value {
        cx.gas.spend(cx.state.gas_params().get(GasId::TransferValueCost).into())?;
    }

    let additional_cold_cost = cx.state.gas_params().cold_account_additional_cost();
    let skip_cold_load = cx.gas.remaining() < additional_cold_cost;
    let account = cx.state.host.load_account(to, true, skip_cold_load)?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    if create_empty_account && transfers_value && account.is_empty {
        cost += u64::from(cx.state.gas_params().get(GasId::NewAccountCost));
    }
    cx.gas.spend(cost)?;

    let mut gas_limit = if cx.state.spec.enables(SpecId::TANGERINE) {
        min(cx.state.gas_params().call_stipend_reduction(cx.gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    cx.gas.spend(gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(cx.state.gas_params().get(GasId::CallStipend).into());
    }

    Ok((gas_limit, account.code))
}

#[inline(always)]
fn call_inner<T: EvmTypes>(
    mut stack: StackMut<'_>,
    mut cx: GasInstructionCx<'_, '_, T>,
    kind: MessageKind,
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
    if cx.state.is_static() && kind == MessageKind::Call && has_transfer {
        return Err(InstrStop::CallNotAllowedInsideStatic);
    }
    if cx.state.message().depth.saturating_add(1) >= Message::CALL_DEPTH_LIMIT {
        stack.push(Word::ZERO)?;
        return Ok(());
    }

    let local_gas_limit = u64::try_from(local_gas_limit).unwrap_or(u64::MAX);
    let (input_range, return_memory_range) = get_memory_input_and_out_ranges(
        &mut cx,
        input_offset,
        input_len,
        return_offset,
        return_len,
    )?;
    let (gas_limit, code) = load_acc_and_calc_gas(
        &mut cx,
        to,
        has_transfer,
        kind == MessageKind::Call,
        local_gas_limit,
    )?;
    let input = memory_range_bytes(&mut cx, input_range)?;

    let current = cx.state.message();
    let (destination, caller, call_value, code_address) = match kind {
        MessageKind::Call => (to, current.destination, value, to),
        MessageKind::CallCode => (current.destination, current.destination, value, to),
        MessageKind::DelegateCall => (current.destination, current.caller, current.value, to),
        MessageKind::StaticCall => (to, current.destination, Word::ZERO, to),
        _ => unreachable!("invalid call message kind"),
    };
    let message = Message {
        kind,
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination,
        caller,
        input,
        value: call_value,
        code_address,
        salt: B256::ZERO,
    };
    let caller_is_static = cx.state.is_static();
    let bytecode = crate::bytecode::Bytecode::new_legacy(code);
    let result =
        cx.state.host.execute_message(cx.state.tx().clone(), bytecode, message, caller_is_static);
    cx.gas.erase_cost(result.gas_remaining);
    let copy_len = min(return_memory_range.len(), result.output.len());
    cx.state.memory().set(return_memory_range.start, &result.output[..copy_len]);
    cx.state.set_return_data(result.output);
    let success = if success(result.stop) { Word::from(1) } else { Word::ZERO };
    stack.push(success)
}

#[instruction(no_stack_preamble, needs_gas)]
pub(crate) fn call(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::Call)
}

#[instruction(no_stack_preamble, needs_gas)]
pub(crate) fn callcode(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::CallCode)
}

#[instruction(no_stack_preamble, needs_gas)]
pub(crate) fn delegatecall(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::DelegateCall)
}

#[instruction(no_stack_preamble, needs_gas)]
pub(crate) fn staticcall(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::StaticCall)
}

#[instruction(no_stack_preamble, needs_gas)]
pub(crate) fn create<const IS_CREATE2: bool>(cx: _) -> Result {
    create_inner(stack, cx, IS_CREATE2)
}

#[inline]
fn create_inner<T: EvmTypes>(
    mut stack: StackMut<'_>,
    mut cx: GasInstructionCx<'_, '_, T>,
    is_create2: bool,
) -> Result {
    require_non_staticcall(cx.state)?;

    let [value, offset, len] = stack.popn::<3>()?;
    let salt = if is_create2 { Some(stack.pop()?) } else { None };
    if cx.state.message().depth.saturating_add(1) >= Message::CALL_DEPTH_LIMIT {
        stack.push(Word::ZERO)?;
        return Ok(());
    }

    let len = word_to_usize(len)?;
    if cx.state.spec.enables(SpecId::SHANGHAI) {
        cx.gas.spend(cx.state.gas_params().initcode_cost(len))?;
    }
    let code_range = resize_memory_range(&mut cx, offset, Word::from(len))?;
    let input = memory_range_bytes(&mut cx, code_range)?;
    let create_cost = if is_create2 {
        cx.state.gas_params().create2_cost(len)
    } else {
        cx.state.gas_params().get(GasId::Create).into()
    };
    cx.gas.spend(create_cost)?;
    let gas_limit = if cx.state.spec.enables(SpecId::TANGERINE) {
        cx.state.gas_params().call_stipend_reduction(cx.gas.remaining())
    } else {
        cx.gas.remaining()
    };
    cx.gas.spend(gas_limit)?;

    let current = cx.state.message();
    let message = Message {
        kind: if is_create2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input: input.clone(),
        value,
        code_address: current.destination,
        salt: salt.map(|salt| B256::from(salt.to_be_bytes())).unwrap_or_default(),
    };
    let bytecode = crate::bytecode::Bytecode::new_legacy(input);
    let result = cx.state.host.execute_message(cx.state.tx().clone(), bytecode, message, false);
    cx.gas.erase_cost(result.gas_remaining);
    if !result.stop.is_success() {
        cx.state.set_return_data(result.output);
    } else {
        cx.state.set_return_data(Bytes::new());
    }
    let address = result
        .created_address
        .filter(|_| success(result.stop))
        .map(|address| Word::from_be_slice(address.as_slice()))
        .unwrap_or_default();
    stack.push(address)
}

#[instruction(needs_gas)]
pub(crate) fn selfdestruct(cx: _, [target]: [Word]) -> Result {
    require_non_staticcall(cx.state)?;
    let target = word_to_address(target);
    let cold_load_gas = cx.state.gas_params().selfdestruct_cold_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let res = cx.state.host.selfdestruct(cx.state.message().destination, target, skip_cold_load)?;
    let should_charge_topup = if cx.state.spec.enables(SpecId::SPURIOUS_DRAGON) {
        res.had_value && !res.target_exists
    } else {
        !res.target_exists
    };
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
        interpreter::{
            InstrStop, Message, MessageKind, MessageResult, Word,
            instructions::tests::{RunConfig, TestHost, push, run},
            op,
        },
        utils::address_to_word,
    };
    use alloy_primitives::{Address, Bytes};

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
                address_to_word(target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host).message(Message {
            destination: caller,
            gas_limit: 10_000,
            ..Default::default()
        }));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(1)]);
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
                address_to_word(target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host).staticcall());
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert!(host.call_static_flags[0]);
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
                address_to_word(code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host).staticcall().gas_limit(20_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
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
                address_to_word(code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interpreter = run(RunConfig::new(code)
            .host(&mut host)
            .message(Message { destination, ..Default::default() })
            .gas_limit(20_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(1)]);
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
                address_to_word(code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::DELEGATECALL, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host).message(Message {
            caller,
            value: Word::from(9),
            gas_limit: 10_000,
            ..Default::default()
        }));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(1)]);
        assert_eq!(host.calls[0].kind, MessageKind::DelegateCall);
        assert_eq!(host.calls[0].caller, caller);
        assert_eq!(host.calls[0].value, Word::from(9));
        assert_eq!(host.calls[0].code_address, code_address);

        let interpreter = run(RunConfig::new([
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
        core::assert_matches!(interpreter.err, InstrStop::OpcodeNotFound);
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
                address_to_word(target),
                Word::from(1000),
            ],
        );
        code.extend([op::STATICCALL, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(1)]);
        assert_eq!(host.calls[0].kind, MessageKind::StaticCall);
        assert!(host.call_static_flags[0]);

        let interpreter = run(RunConfig::new([
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
        core::assert_matches!(interpreter.err, InstrStop::OpcodeNotFound);
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

        let interpreter = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(created)]);
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

        let interpreter = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(created), Word::ZERO]);
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

        let interpreter = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::ZERO, Word::from(2)]);
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

        let interpreter = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(created)]);
        assert_eq!(host.calls[0].kind, MessageKind::Create2);

        let interpreter = run(RunConfig::new([
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
        core::assert_matches!(interpreter.err, InstrStop::OpcodeNotFound);
    }

    #[test]
    fn selfdestruct_opcode() {
        let contract = Address::from([0x11; 20]);
        let target = Address::from([0x99; 20]);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(target));
        code.push(op::SELFDESTRUCT);

        let interpreter = run(RunConfig::new(code).host(&mut host).message(Message {
            destination: contract,
            gas_limit: 10_000,
            ..Default::default()
        }));
        core::assert_matches!(interpreter.err, InstrStop::SelfDestruct);
        assert_eq!(host.selfdestructs, [(contract, target, false)]);
    }
}
