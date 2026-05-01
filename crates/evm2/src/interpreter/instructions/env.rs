use super::utils::{address_to_word, as_usize, as_usize_saturated, b256_to_word, check_spec};
use crate::{
    AccountLoad,
    interpreter::{InstrStop, Result, SpecId, Word, memory::resize_memory, table::InstructionCx},
};
use alloy_primitives::B256;
use evm2_macros::instruction;

fn load_account(
    cx: &mut InstructionCx<'_, '_, '_>,
    addr: Word,
    load_code: bool,
) -> Result<AccountLoad> {
    let cold_load_gas = cx.state.gas_params.cold_account_additional_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let account = cx.state.host.load_account(addr, load_code, skip_cold_load)?;
    if account.is_cold {
        cx.gas.spend(cold_load_gas)?;
    }
    Ok(account)
}

#[instruction]
pub(in crate::interpreter) fn address(cx: _) -> out {
    *out = address_to_word(cx.state.message.destination);
}

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, [addr]: [Word]) -> Result<out> {
    *out = load_account(&mut cx, addr, false)?.balance;
}

#[instruction]
pub(in crate::interpreter) fn origin(cx: _) -> out {
    *out = address_to_word(cx.state.tx.origin);
}

#[instruction]
pub(in crate::interpreter) fn caller(cx: _) -> out {
    *out = address_to_word(cx.state.message.caller);
}

#[instruction]
pub(in crate::interpreter) fn callvalue(cx: _) -> out {
    *out = cx.state.message.value;
}

#[instruction]
pub(in crate::interpreter) fn calldataload(cx: _, [offset]: [Word]) -> out {
    let offset = as_usize_saturated(offset);
    let input = cx.state.message.input.as_ref();
    let mut word = B256::ZERO;
    if offset < input.len() {
        let len = 32.min(input.len() - offset);
        word[..len].copy_from_slice(&input[offset..offset + len]);
    }
    *out = b256_to_word(word);
}

#[instruction]
pub(in crate::interpreter) fn calldatasize(cx: _) -> out {
    *out = Word::from(cx.state.message.input.len());
}

#[instruction]
pub(in crate::interpreter) fn calldatacopy(
    cx: _,
    [memory_offset, data_offset, len]: [Word],
) -> Result {
    let len = as_usize(len)?;
    if len == 0 {
        return Ok(());
    }
    let memory_offset = as_usize(memory_offset)?;
    let data_offset = as_usize_saturated(data_offset);
    resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, data_offset, len, &cx.state.message.input)
}

#[instruction]
pub(in crate::interpreter) fn codesize(cx: _) -> out {
    *out = Word::from(cx.state.bytecode.len());
}

#[instruction]
pub(in crate::interpreter) fn codecopy(cx: _, [memory_offset, code_offset, len]: [Word]) -> Result {
    let len = as_usize(len)?;
    if len == 0 {
        return Ok(());
    }
    let memory_offset = as_usize(memory_offset)?;
    let code_offset = as_usize_saturated(code_offset);
    resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, code_offset, len, cx.state.bytecode.as_slice())
}

#[instruction]
pub(in crate::interpreter) fn gasprice(cx: _) -> out {
    *out = cx.state.tx.gas_price;
}

#[instruction]
pub(in crate::interpreter) fn extcodesize(cx: _, [addr]: [Word]) -> Result<out> {
    *out = Word::from(load_account(&mut cx, addr, true)?.code.len());
}

#[instruction]
pub(in crate::interpreter) fn extcodehash(cx: _, [addr]: [Word]) -> Result<out> {
    check_spec(cx.state.spec, SpecId::CONSTANTINOPLE)?;
    let account = load_account(&mut cx, addr, false)?;
    *out = if account.is_empty { Word::ZERO } else { b256_to_word(account.code_hash) };
}

#[instruction]
pub(in crate::interpreter) fn extcodecopy(
    cx: _,
    [addr, memory_offset, code_offset, len]: [Word],
) -> Result {
    let len = as_usize(len)?;
    cx.gas.spend(cx.state.gas_params.extcodecopy_cost(len))?;

    let mut memory_offset_usize = 0;
    if len != 0 {
        memory_offset_usize = as_usize(memory_offset)?;
        resize_memory(cx.gas, cx.state.memory, memory_offset_usize, len)?;
    }

    let code = load_account(&mut cx, addr, true)?.code;
    let code_offset = as_usize_saturated(code_offset).min(code.len());
    cx.state.memory.set_data(memory_offset_usize, code_offset, len, &code)
}

#[instruction]
pub(in crate::interpreter) fn returndatasize(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::BYZANTIUM)?;
    *out = Word::from(cx.state.return_data.len());
}

#[instruction]
pub(in crate::interpreter) fn returndatacopy(
    cx: _,
    [memory_offset, data_offset, len]: [Word],
) -> Result {
    check_spec(cx.state.spec, SpecId::BYZANTIUM)?;
    let len = as_usize(len)?;
    let data_offset = as_usize_saturated(data_offset);
    if data_offset.saturating_add(len) > cx.state.return_data.len() {
        return Err(InstrStop::OutOfOffset);
    }

    cx.gas.spend(cx.state.gas_params.copy_cost(len))?;
    if len == 0 {
        return Ok(());
    }

    let memory_offset = as_usize(memory_offset)?;
    resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, data_offset, len, cx.state.return_data)
}

#[cfg(test)]
mod tests {
    use crate::{
        env::TxEnv,
        interpreter::{
            InstrStop, Message, SpecId, Word,
            instructions::{
                tests::{RunConfig, TestHost, assert_stack, push, run, run_stack},
                utils::{address_to_word, b256_to_word},
            },
            op,
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256, Bytes};

    fn neg(value: u64) -> Word {
        Word::from(0).wrapping_sub(Word::from(value))
    }

    fn test_message() -> Message {
        Message { gas_limit: 10_000, ..Message::default() }
    }

    fn stack_code<const N: usize>(inputs: [Word; N], opcode: u8) -> Vec<u8> {
        let mut code = Vec::new();
        for input in inputs.into_iter().rev() {
            push(&mut code, input);
        }
        code.extend([opcode, op::STOP]);
        code
    }

    #[test]
    fn address_opcode() {
        let address = Address::from([0x11; 20]);
        let mut host = TestHost::default();
        let message = Message { destination: address, ..test_message() };
        let interpreter =
            run(RunConfig::new([op::ADDRESS, op::STOP]).host(&mut host).message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(address)]);
    }

    #[test]
    fn balance_opcode() {
        assert_stack!(BALANCE(0xbeef), 0xbeef);
        assert_stack!(BALANCE(0), 0);
        assert_stack!(BALANCE(neg(1)), neg(1));
    }

    #[test]
    fn balance_cold_account_cost() {
        let mut host = TestHost { is_cold: true, ..TestHost::default() };
        let interpreter = run(RunConfig::new([op::PUSH1, 0xbe, op::BALANCE, op::STOP])
            .host(&mut host)
            .spec(SpecId::BERLIN));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbe)]);
        assert_eq!(interpreter.gas_remaining(), 7_397);
    }

    #[test]
    fn balance_cold_account_skip_oog() {
        let mut host = TestHost { is_cold: true, ..TestHost::default() };
        let interpreter = run(RunConfig::new([op::PUSH1, 0xbe, op::BALANCE, op::STOP])
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .gas_limit(103));
        core::assert_matches!(interpreter.err, InstrStop::OutOfGas);
    }

    #[test]
    fn origin_opcode() {
        let origin = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let tx_env = TxEnv { origin, ..TxEnv::default() };
        let interpreter =
            run(RunConfig::new([op::ORIGIN, op::STOP]).host(&mut host).tx_env(tx_env));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(origin)]);
    }

    #[test]
    fn caller_opcode() {
        let caller = Address::from([0x33; 20]);
        let mut host = TestHost::default();
        let message = Message { caller, ..test_message() };
        let interpreter =
            run(RunConfig::new([op::CALLER, op::STOP]).host(&mut host).message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(caller)]);
    }

    #[test]
    fn callvalue_opcode() {
        let mut host = TestHost::default();
        let message = Message { value: Word::from(0xbeef), ..test_message() };
        let interpreter =
            run(RunConfig::new([op::CALLVALUE, op::STOP]).host(&mut host).message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);
    }

    #[test]
    fn calldataload_opcode() {
        let input = Bytes::from(Vec::from([1_u8, 2, 3]));
        let mut host = TestHost::default();
        let message = Message { input, ..test_message() };

        let interpreter = run(RunConfig::new([op::PUSH0, op::CALLDATALOAD, op::STOP])
            .host(&mut host)
            .message(message.clone()));
        let mut expected = [0_u8; 32];
        expected[..3].copy_from_slice(&[1, 2, 3]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run(RunConfig::new([op::PUSH1, 0x20, op::CALLDATALOAD, op::STOP])
            .host(&mut host)
            .message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn calldatasize_opcode() {
        let input = Bytes::from(Vec::from([1_u8, 2, 3, 4]));
        let mut host = TestHost::default();
        let message = Message { input, ..test_message() };
        let interpreter =
            run(RunConfig::new([op::CALLDATASIZE, op::STOP]).host(&mut host).message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(4)]);
    }

    #[test]
    fn calldatacopy_opcode() {
        let input = Bytes::from(Vec::from([0xaa_u8, 0xbb, 0xcc]));
        let mut host = TestHost::default();
        let message = Message { input, ..test_message() };
        let mut code = Vec::new();
        push(&mut code, 2);
        push(&mut code, 1);
        push(&mut code, 0);
        code.push(op::CALLDATACOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(RunConfig::new(code).host(&mut host).message(message.clone()));
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::MAX, Word::MAX, Word::from(0)],
            op::CALLDATACOPY,
        ))
        .host(&mut host)
        .message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
    }

    #[test]
    fn codesize_opcode() {
        let interpreter = run(RunConfig::new([op::CODESIZE, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(2)]);

        let interpreter = run(RunConfig::new([op::PUSH1, 0x00, op::CODESIZE, op::STOP]));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0), Word::from(4)]);
    }

    #[test]
    fn codecopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, Word::from(2));
        push(&mut code, Word::from(5));
        push(&mut code, 0);
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(RunConfig::new(code));
        let mut expected = [0u8; 32];
        expected[..2].copy_from_slice(&[op::CODECOPY, op::PUSH0]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let mut code = Vec::new();
        push(&mut code, Word::from(1));
        push(&mut code, Word::from(usize::MAX));
        push(&mut code, 0);
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);
        let interpreter = run(RunConfig::new(code));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run_stack([Word::MAX, Word::MAX, Word::from(0)], op::CODECOPY);
        core::assert_matches!(interpreter.err, InstrStop::Stop);

        let interpreter = run_stack([Word::MAX, Word::from(0), Word::from(1)], op::CODECOPY);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn gasprice_opcode() {
        let mut host = TestHost::default();
        let tx_env = TxEnv { gas_price: Word::from(0x1234), ..TxEnv::default() };
        let interpreter =
            run(RunConfig::new([op::GASPRICE, op::STOP]).host(&mut host).tx_env(tx_env));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0x1234)]);
    }

    #[test]
    fn extcodesize_opcode() {
        let mut host = TestHost { code: Bytes::from(vec![0; 0x42]), ..TestHost::default() };
        let interpreter =
            run(RunConfig::new([op::PUSH1, 0xbe, op::EXTCODESIZE, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0x42)]);
    }

    #[test]
    fn extcodecopy_opcode() {
        let mut host =
            TestHost { code: Bytes::from_static(&[0xaa, 0xbb, 0xcc]), ..TestHost::default() };
        let mut code = Vec::new();
        push(&mut code, 2);
        push(&mut code, 1);
        push(&mut code, 0);
        push(&mut code, 0xbeef);
        code.push(op::EXTCODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(RunConfig::new(code).host(&mut host));
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let mut code = Vec::new();
        push(&mut code, 4);
        push(&mut code, 2);
        push(&mut code, 0);
        push(&mut code, 0xbeef);
        code.push(op::EXTCODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);
        let interpreter = run(RunConfig::new(code).host(&mut host));
        let mut expected = [0_u8; 32];
        expected[..1].copy_from_slice(&[0xcc]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::from(0xbeef), Word::MAX, Word::MAX, Word::from(0)],
            op::EXTCODECOPY,
        ))
        .host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::from(0xbeef), Word::MAX, Word::from(0), Word::from(1)],
            op::EXTCODECOPY,
        ))
        .host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn returndatasize_opcode() {
        let interpreter = run(RunConfig::new([op::RETURNDATASIZE, op::STOP])
            .spec(SpecId::BYZANTIUM)
            .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(3)]);

        let interpreter = run(RunConfig::new([op::RETURNDATASIZE]));
        core::assert_matches!(interpreter.err, InstrStop::NotActivated);
    }

    #[test]
    fn returndatacopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, 2);
        push(&mut code, 1);
        push(&mut code, 0);
        code.push(op::RETURNDATACOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(RunConfig::new(code)
            .spec(SpecId::BYZANTIUM)
            .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::from(0), Word::from(3), Word::from(0)],
            op::RETURNDATACOPY,
        ))
        .spec(SpecId::BYZANTIUM)
        .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        core::assert_matches!(interpreter.err, InstrStop::Stop);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::from(0), Word::from(4), Word::from(0)],
            op::RETURNDATACOPY,
        ))
        .spec(SpecId::BYZANTIUM)
        .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        core::assert_matches!(interpreter.err, InstrStop::OutOfOffset);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::MAX, Word::from(0), Word::from(1)],
            op::RETURNDATACOPY,
        ))
        .spec(SpecId::BYZANTIUM)
        .return_data(Bytes::from_static(&[0xaa])));
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);

        let interpreter = run(RunConfig::new(stack_code(
            [Word::from(0), Word::from(0), Word::from(0)],
            op::RETURNDATACOPY,
        )));
        core::assert_matches!(interpreter.err, InstrStop::NotActivated);
    }

    #[test]
    fn extcodehash_opcode() {
        let hash = B256::with_last_byte(0x77);
        let mut host = TestHost { code_hash: hash, ..TestHost::default() };
        let interpreter = run(RunConfig::new([op::PUSH1, 0xbe, op::EXTCODEHASH, op::STOP])
            .host(&mut host)
            .spec(SpecId::CONSTANTINOPLE));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(hash)]);

        let interpreter =
            run(RunConfig::new([op::PUSH1, 0xbe, op::EXTCODEHASH, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::NotActivated);
    }
}
