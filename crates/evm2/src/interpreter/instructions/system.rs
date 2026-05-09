//! System opcode implementations.

use crate::{
    EvmTypes, SpecId,
    constants::{CALL_DEPTH_LIMIT, EIP7702_BYTECODE_LEN, EIP7702_MAGIC_BYTES, EIP7702_VERSION},
    interpreter::{
        Host, InstrStop, InterpreterState, Message, MessageKind, MessageResult, Result, StackMut,
        Word, memory::resize_memory, private::GasInstructionCx,
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
const fn success(stop: InstrStop) -> bool {
    matches!(stop, InstrStop::Stop | InstrStop::Return | InstrStop::SelfDestruct)
}

#[inline]
const fn should_charge_new_account_gas(
    spec: SpecId,
    transfers_value: bool,
    target_is_empty_for_new_account_gas: bool,
) -> bool {
    target_is_empty_for_new_account_gas
        && (!spec.enables(SpecId::SPURIOUS_DRAGON) || transfers_value)
}

#[inline]
fn call_too_deep_result(gas_limit: u64) -> MessageResult {
    MessageResult { stop: InstrStop::CallTooDeep, gas_remaining: gas_limit, ..Default::default() }
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

fn eip7702_address(code: &Bytes) -> Option<Address> {
    if code.len() == EIP7702_BYTECODE_LEN
        && code.starts_with(EIP7702_MAGIC_BYTES)
        && code[2] == EIP7702_VERSION
    {
        return Some(Address::from_slice(&code[3..]));
    }
    None
}

fn load_acc_and_calc_gas<T: EvmTypes>(
    cx: &mut GasInstructionCx<'_, '_, T>,
    to: Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytes, Address, bool)> {
    if transfers_value {
        cx.gas.spend(cx.state.gas_params().get(GasId::TransferValueCost).into())?;
    }

    let additional_cold_cost = cx.state.gas_params().cold_account_additional_cost();
    let remaining_gas = cx.gas.remaining();
    let skip_cold_load = remaining_gas < additional_cold_cost;
    let account = cx.state.host().load_account(to, true, skip_cold_load)?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    let mut code = account.code;
    let mut code_address = to;
    if cx.state.spec().enables(SpecId::PRAGUE)
        && let Some(delegated_address) = eip7702_address(&code)
    {
        cost += u64::from(cx.state.gas_params().get(GasId::WarmStorageReadCost));
        if cost > remaining_gas {
            return Err(InstrStop::OutOfGas);
        }
        let skip_cold_load = remaining_gas < cost.saturating_add(additional_cold_cost);
        let delegated_account =
            cx.state.host().load_account(delegated_address, true, skip_cold_load)?;
        if delegated_account.is_cold {
            cost += additional_cold_cost;
        }
        code = delegated_account.code;
        code_address = delegated_address;
    }
    let spec = cx.state.spec();
    if create_empty_account
        && should_charge_new_account_gas(
            spec,
            transfers_value,
            cx.state.host().target_is_empty_for_new_account_gas(to, spec),
        )
    {
        cost += u64::from(cx.state.gas_params().get(GasId::NewAccountCost));
    }
    cx.gas.spend(cost)?;

    let mut gas_limit = if cx.state.spec().enables(SpecId::TANGERINE) {
        min(cx.state.gas_params().call_stipend_reduction(cx.gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    cx.gas.spend(gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(cx.state.gas_params().get(GasId::CallStipend).into());
    }

    let disable_precompiles = code_address != to;
    Ok((gas_limit, code, code_address, disable_precompiles))
}

#[inline(never)]
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

    let local_gas_limit = u64::try_from(local_gas_limit).unwrap_or(u64::MAX);
    let (input_range, return_memory_range) = get_memory_input_and_out_ranges(
        &mut cx,
        input_offset,
        input_len,
        return_offset,
        return_len,
    )?;
    let (gas_limit, code, resolved_code_address, disable_precompiles) = load_acc_and_calc_gas(
        &mut cx,
        to,
        has_transfer,
        kind == MessageKind::Call,
        local_gas_limit,
    )?;
    let input = memory_range_bytes(&mut cx, input_range)?;

    let current = cx.state.message();
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
    let mut message = Message {
        kind,
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination,
        caller,
        input,
        value: call_value,
        code_address,
        disable_precompiles,
        salt: B256::ZERO,
    };
    let caller_is_static = cx.state.is_static();
    let mut result = if let Some(result) = cx.state.inspect_call(&mut message) {
        result
    } else if message.depth > CALL_DEPTH_LIMIT {
        call_too_deep_result(message.gas_limit)
    } else {
        let bytecode = crate::bytecode::Bytecode::new_legacy(code);
        let tx_env = unsafe { crate::trustme::decouple_lt(cx.state.tx()) };
        cx.state.host().execute_message(tx_env, bytecode, &message, caller_is_static)
    };
    cx.state.inspect_call_end(&message, &mut result);
    cx.gas.erase_cost(result.gas_returned_to_parent());
    cx.gas.record_refund(result.refund_propagated_to_parent());
    let copy_len = min(return_memory_range.len(), result.output.len());
    cx.state.memory().set(return_memory_range.start, &result.output[..copy_len]);
    cx.state.set_return_data(result.output);
    let success = if success(result.stop) { Word::from(1) } else { Word::ZERO };
    stack.push(success)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn call(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::Call)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn callcode(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::CallCode)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn delegatecall(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::DelegateCall)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn staticcall(cx: _) -> Result {
    call_inner(stack, cx, MessageKind::StaticCall)
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub(crate) fn create<const IS_CREATE2: bool>(cx: _) -> Result {
    create_inner(stack, cx, IS_CREATE2)
}

#[inline(never)]
fn create_inner<T: EvmTypes>(
    mut stack: StackMut<'_>,
    mut cx: GasInstructionCx<'_, '_, T>,
    is_create2: bool,
) -> Result {
    require_non_staticcall(cx.state)?;

    let [value, offset, len] = stack.popn::<3>()?;
    let salt = if is_create2 { Some(stack.pop()?) } else { None };

    let len = word_to_usize(len)?;
    if cx.state.spec().enables(SpecId::SHANGHAI) {
        if len > cx.state.version().max_initcode_size {
            return Err(InstrStop::CreateInitCodeSizeLimit);
        }
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
    let gas_limit = if cx.state.spec().enables(SpecId::TANGERINE) {
        cx.state.gas_params().call_stipend_reduction(cx.gas.remaining())
    } else {
        cx.gas.remaining()
    };
    cx.gas.spend(gas_limit)?;

    let current = cx.state.message();
    let mut message = Message {
        kind: if is_create2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input: input.clone(),
        value,
        code_address: current.destination,
        disable_precompiles: false,
        salt: salt.map(|salt| B256::from(salt.to_be_bytes())).unwrap_or_default(),
    };
    let mut result = if let Some(result) = cx.state.inspect_create(&mut message) {
        result
    } else if message.depth > CALL_DEPTH_LIMIT {
        call_too_deep_result(message.gas_limit)
    } else {
        let bytecode = crate::bytecode::Bytecode::new_legacy(input);
        let tx_env = unsafe { crate::trustme::decouple_lt(cx.state.tx()) };
        cx.state.host().execute_message(tx_env, bytecode, &message, false)
    };
    cx.state.inspect_create_end(&message, &mut result);
    cx.gas.erase_cost(result.gas_returned_to_parent());
    cx.gas.record_refund(result.refund_propagated_to_parent());
    // EIP-211 exposes CREATE failure data only for REVERT; other failures clear returndata.
    if result.stop == InstrStop::Revert {
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

#[instruction(dynamic_gas)]
pub(crate) fn selfdestruct(cx: _, [target]: [Word]) -> Result {
    require_non_staticcall(cx.state)?;
    let target = word_to_address(target);
    let cold_load_gas = cx.state.gas_params().selfdestruct_cold_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let destination = cx.state.message().destination;
    let res = cx.state.host().selfdestruct(destination, target, skip_cold_load)?;
    cx.state.inspect_selfdestruct(destination, target, res.value);
    let should_charge_topup =
        should_charge_new_account_gas(cx.state.spec(), res.had_value, res.target_is_empty);
    cx.gas.spend(cx.state.gas_params().selfdestruct_cost(should_charge_topup, res.is_cold))?;
    if !res.previously_destroyed {
        cx.gas.record_refund(cx.state.gas_params().get(GasId::SelfdestructRefund) as i64);
    }
    Err(InstrStop::SelfDestruct)
}

#[cfg(test)]
mod tests {
    use crate::{
        BaseEvmConfigSelector, ExecutionConfig, SpecId,
        bytecode::Bytecode,
        constants::{CALL_DEPTH_LIMIT, MAX_INITCODE_SIZE},
        evm::inspector::Inspector,
        interpreter::{
            InstrStop, Interpreter, Message, MessageKind, MessageResult, Word,
            instructions::tests::{RunConfig, TestHost, TestTypes, push, run},
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

    #[derive(Default)]
    struct MessageInspector {
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
        create_depth: Option<u16>,
        create_end_stop: Option<InstrStop>,
        selfdestruct: Option<(Address, Address, Word)>,
    }

    impl Inspector<TestTypes> for MessageInspector {
        fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
            self.call_depth = Some(message.depth);
            None
        }

        fn call_end(&mut self, _message: &Message, result: &mut MessageResult) {
            self.call_end_stop = Some(result.stop);
        }

        fn create(&mut self, message: &mut Message) -> Option<MessageResult> {
            self.create_depth = Some(message.depth);
            None
        }

        fn create_end(&mut self, _message: &Message, result: &mut MessageResult) {
            self.create_end_stop = Some(result.stop);
        }

        fn selfdestruct(&mut self, contract: Address, target: Address, value: Word) {
            self.selfdestruct = Some((contract, target, value));
        }
    }

    fn run_with_message_inspector(
        code: Vec<u8>,
        host: &mut TestHost,
        message: &Message,
        gas_limit: u64,
        inspector: &mut MessageInspector,
    ) -> (InstrStop, Vec<Word>) {
        let tx_env = crate::env::TxEnv::default();
        let bytecode = Bytecode::new_legacy(Bytes::from(code));
        let mut message = message.clone();
        message.gas_limit = gas_limit;
        let mut inner = Interpreter::<TestTypes>::new(bytecode, &tx_env, &message, false);
        let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let stop = inner.run_inspect(&config, host, inspector);
        let stack = inner.stack().to_vec();
        (stop, stack)
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
                address_to_word(target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interpreter = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::ZERO]);
        assert_eq!(interpreter.gas_remaining(), 15_679);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_too_deep_is_inspected_without_host_call() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                address_to_word(target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let (stop, stack) = run_with_message_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.call_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::CallTooDeep));
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

        let interpreter = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.gas_remaining(), 42_553);
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
                address_to_word(target),
                Word::ZERO,
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interpreter = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::ZERO]);
        assert_eq!(interpreter.gas_remaining(), 24_279);
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
                address_to_word(target),
                Word::ZERO,
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interpreter = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::ZERO]);
        assert_eq!(interpreter.gas_remaining(), 49_279);
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
                address_to_word(code_address),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host).staticcall().gas_limit(20_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::ZERO, Word::from(2)]);
    }

    #[test]
    fn create_too_deep_charges_create_gas() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let interpreter = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .message(Message { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::ZERO]);
        assert_eq!(interpreter.gas_remaining(), 17_991);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_too_deep_is_inspected_without_host_call() {
        let mut host = TestHost::default();
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let (stop, stack) = run_with_message_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::CallTooDeep));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_initcode_size_limit_halts_after_shanghai() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(MAX_INITCODE_SIZE + 1), Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let interpreter =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::SHANGHAI).gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::CreateInitCodeSizeLimit));
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

        let interpreter = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert!(matches!(interpreter.err, InstrStop::Stop));
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
        assert!(matches!(interpreter.err, InstrStop::OpcodeNotFound));
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
        assert!(matches!(interpreter.err, InstrStop::SelfDestruct));
        assert_eq!(host.selfdestructs, [(contract, target, false)]);
    }

    #[test]
    fn selfdestruct_is_inspected_from_opcode() {
        let contract = Address::from([0x11; 20]);
        let target = Address::from([0x99; 20]);
        let value = Word::from(0xbeef);
        let mut host = TestHost {
            selfdestruct_result: crate::evm::SelfDestructResult {
                had_value: true,
                value,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(target));
        code.push(op::SELFDESTRUCT);

        let (stop, _) = run_with_message_inspector(
            code,
            &mut host,
            &Message { destination: contract, gas_limit: 10_000, ..Default::default() },
            10_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::SelfDestruct));
        assert_eq!(inspector.selfdestruct, Some((contract, target, value)));
    }
}
