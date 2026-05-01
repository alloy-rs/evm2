//! System opcode implementations.

use super::utils::{address_to_word, as_usize, word_to_address};
use crate::{
    EvmConfig,
    interpreter::{
        GasId, Host, InstrStop, Message, MessageKind, Result, SpecId, Word, memory::resize_memory,
        table::InstructionCx,
    },
};
use alloy_primitives::{Address, Bytes};
use core::{cmp::min, ops::Range};
use evm2_macros::instruction;

const fn require_non_staticcall<C: EvmConfig>(cx: &InstructionCx<'_, '_, C>) -> Result {
    if cx.state.message.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    Ok(())
}

#[inline]
const fn success(stop: InstrStop) -> bool {
    matches!(stop, InstrStop::Stop | InstrStop::Return | InstrStop::SelfDestruct)
}

fn resize_memory_range<C: EvmConfig>(
    cx: &mut InstructionCx<'_, '_, C>,
    offset: Word,
    len: Word,
) -> Result<Range<usize>> {
    let len = as_usize(len)?;
    let offset = if len != 0 {
        let offset = as_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
        offset
    } else {
        usize::MAX
    };
    Ok(offset..offset + len)
}

fn get_memory_input_and_out_ranges<C: EvmConfig>(
    cx: &mut InstructionCx<'_, '_, C>,
    input_offset: Word,
    input_len: Word,
    return_offset: Word,
    return_len: Word,
) -> Result<(Range<usize>, Range<usize>)> {
    let input = resize_memory_range(cx, input_offset, input_len)?;
    let output = resize_memory_range(cx, return_offset, return_len)?;
    Ok((input, output))
}

fn memory_range_bytes<C: EvmConfig>(
    cx: &mut InstructionCx<'_, '_, C>,
    range: Range<usize>,
) -> Result<Bytes> {
    if range.is_empty() {
        return Ok(Bytes::new());
    }
    Ok(Bytes::copy_from_slice(cx.state.memory.slice(range.start, range.len())?))
}

fn load_acc_and_calc_gas<C: EvmConfig>(
    cx: &mut InstructionCx<'_, '_, C>,
    to: Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytes)> {
    if transfers_value {
        cx.gas.spend(cx.gas_params.get(GasId::TransferValueCost))?;
    }

    let additional_cold_cost = cx.gas_params.cold_account_additional_cost();
    let skip_cold_load = cx.gas.remaining() < additional_cold_cost;
    let account = cx.state.host.load_account(address_to_word(to), true, skip_cold_load)?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    if create_empty_account && transfers_value && account.is_empty {
        cost += cx.gas_params.get(GasId::NewAccountCost);
    }
    cx.gas.spend(cost)?;

    let mut gas_limit = if cx.state.spec.enables(SpecId::TANGERINE) {
        min(cx.gas_params.call_stipend_reduction(cx.gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    cx.gas.spend(gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(cx.gas_params.get(GasId::CallStipend));
    }

    Ok((gas_limit, account.code))
}

struct CallArgs {
    kind: MessageKind,
    local_gas_limit: Word,
    to: Word,
    value: Word,
    input_offset: Word,
    input_len: Word,
    return_offset: Word,
    return_len: Word,
}

fn call_inner<C: EvmConfig>(mut cx: InstructionCx<'_, '_, C>, args: CallArgs) -> Result<Word> {
    let CallArgs {
        kind,
        local_gas_limit,
        to,
        value,
        input_offset,
        input_len,
        return_offset,
        return_len,
    } = args;
    let to = word_to_address(to);
    let has_transfer = !value.is_zero();
    if cx.state.message.is_static() && has_transfer {
        return Err(InstrStop::CallNotAllowedInsideStatic);
    }

    let local_gas_limit = u64::try_from(local_gas_limit).unwrap_or(u64::MAX);
    let (input_range, _return_memory_offset) = get_memory_input_and_out_ranges(
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

    let current = cx.state.message;
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
    };
    let bytecode = crate::bytecode::Bytecode::new_legacy(code);
    match cx.state.host.execute_message(cx.state.tx.clone(), bytecode, message) {
        Ok(_) => Ok(Word::from(1)),
        Err(stop) if success(stop) => Ok(Word::from(1)),
        Err(_) => Ok(Word::ZERO),
    }
}

#[instruction(raw)]
pub(in crate::interpreter) fn call(cx: _) -> Result {
    let [local_gas_limit, to, value, input_offset, input_len, return_offset, return_len] =
        stack.popn::<7>()?;
    let result = call_inner(
        cx,
        CallArgs {
            kind: MessageKind::Call,
            local_gas_limit,
            to,
            value,
            input_offset,
            input_len,
            return_offset,
            return_len,
        },
    )?;
    stack.push(result)?;
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn callcode(cx: _) -> Result {
    let [local_gas_limit, to, value, input_offset, input_len, return_offset, return_len] =
        stack.popn::<7>()?;
    let result = call_inner(
        cx,
        CallArgs {
            kind: MessageKind::CallCode,
            local_gas_limit,
            to,
            value,
            input_offset,
            input_len,
            return_offset,
            return_len,
        },
    )?;
    stack.push(result)?;
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn delegatecall(cx: _) -> Result {
    let [local_gas_limit, to, input_offset, input_len, return_offset, return_len] =
        stack.popn::<6>()?;
    let result = call_inner(
        cx,
        CallArgs {
            kind: MessageKind::DelegateCall,
            local_gas_limit,
            to,
            value: Word::ZERO,
            input_offset,
            input_len,
            return_offset,
            return_len,
        },
    )?;
    stack.push(result)?;
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn staticcall(cx: _) -> Result {
    let [local_gas_limit, to, input_offset, input_len, return_offset, return_len] =
        stack.popn::<6>()?;
    let result = call_inner(
        cx,
        CallArgs {
            kind: MessageKind::StaticCall,
            local_gas_limit,
            to,
            value: Word::ZERO,
            input_offset,
            input_len,
            return_offset,
            return_len,
        },
    )?;
    stack.push(result)?;
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn create<const IS_CREATE2: bool>(cx: _) -> Result {
    require_non_staticcall(&cx)?;

    let [value, offset, len] = stack.popn::<3>()?;
    let salt = if IS_CREATE2 { Some(stack.pop()?) } else { None };
    let len = as_usize(len)?;
    if cx.state.spec.enables(SpecId::SHANGHAI) {
        cx.gas.spend(cx.gas_params.initcode_cost(len))?;
    }
    let code_range = resize_memory_range(&mut cx, offset, Word::from(len))?;
    let input = memory_range_bytes(&mut cx, code_range)?;
    let create_cost =
        if IS_CREATE2 { cx.gas_params.create2_cost(len) } else { cx.gas_params.get(GasId::Create) };
    cx.gas.spend(create_cost)?;
    let gas_limit = if cx.state.spec.enables(SpecId::TANGERINE) {
        cx.gas_params.call_stipend_reduction(cx.gas.remaining())
    } else {
        cx.gas.remaining()
    };
    cx.gas.spend(gas_limit)?;

    let current = cx.state.message;
    let message = Message {
        kind: if IS_CREATE2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input: input.clone(),
        value,
        code_address: current.destination,
    };
    let bytecode = crate::bytecode::Bytecode::new_legacy(input);
    let result = cx.state.host.execute_message(cx.state.tx.clone(), bytecode, message)?;
    let _ = salt;
    stack.push(result)
}

#[instruction]
pub(in crate::interpreter) fn selfdestruct(cx: _, [target]: [Word]) -> Result {
    require_non_staticcall(&cx)?;
    let target = word_to_address(target);
    let cold_load_gas = cx.gas_params.selfdestruct_cold_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let res = cx.state.host.selfdestruct(cx.state.message.destination, target, skip_cold_load)?;
    let should_charge_topup = if cx.state.spec.enables(SpecId::SPURIOUS_DRAGON) {
        res.had_value && !res.target_exists
    } else {
        !res.target_exists
    };
    cx.gas.spend(cx.gas_params.selfdestruct_cost(should_charge_topup, res.is_cold))?;
    if !res.previously_destroyed {
        cx.gas.record_refund(cx.gas_params.get(GasId::SelfdestructRefund) as i64);
    }
    Err(InstrStop::SelfDestruct)
}

#[cfg(test)]
mod tests {
    use super::address_to_word;
    use crate::interpreter::{
        InstrStop, Message, MessageKind, SpecId, Word,
        instructions::tests::{RunConfig, TestHost, push, run},
        op,
    };
    use alloy_primitives::Address;

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
        assert!(host.calls[0].is_static());

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
        let mut host =
            TestHost { execute_result: Ok(address_to_word(created)), ..Default::default() };
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
    fn create2_opcode() {
        let created = Address::from([0x88; 20]);
        let mut host =
            TestHost { execute_result: Ok(address_to_word(created)), ..Default::default() };
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
