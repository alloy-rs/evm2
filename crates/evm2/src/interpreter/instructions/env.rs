use super::utils::{address_to_word, as_usize, as_usize_saturated, b256_to_word, check_spec};
use crate::interpreter::{SpecId, Word, memory::resize_memory};
use alloy_primitives::B256;
use evm2_macros::instruction;

#[instruction]
pub(in crate::interpreter) fn address(cx: _) -> out {
    *out = address_to_word(cx.state.message.destination);
}

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, [addr]: [Word]) -> out {
    *out = cx.state.host.balance(addr);
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
pub(in crate::interpreter) fn extcodesize(cx: _, [addr]: [Word]) -> out {
    *out = Word::from(cx.state.host.get_code_size(addr));
}

#[instruction]
pub(in crate::interpreter) fn extcodehash(cx: _, [addr]: [Word]) -> Result<out> {
    check_spec(cx.state.spec, SpecId::CONSTANTINOPLE)?;
    *out = b256_to_word(cx.state.host.get_code_hash(addr));
}

#[cfg(test)]
mod tests {
    use crate::{
        env::TxEnv,
        interpreter::{
            InstrStop, Message, SpecId, Word,
            instructions::{
                tests::{
                    TestHost, assert_stack, push, run, run_stack, run_with_host,
                    run_with_host_and_spec, run_with_host_message, run_with_host_tx_env,
                },
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
        let interpreter = run_with_host_message([op::ADDRESS, op::STOP], &mut host, message);
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
    fn origin_opcode() {
        let origin = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let tx_env = TxEnv { origin, ..TxEnv::default() };
        let interpreter = run_with_host_tx_env([op::ORIGIN, op::STOP], &mut host, tx_env);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(origin)]);
    }

    #[test]
    fn caller_opcode() {
        let caller = Address::from([0x33; 20]);
        let mut host = TestHost::default();
        let message = Message { caller, ..test_message() };
        let interpreter = run_with_host_message([op::CALLER, op::STOP], &mut host, message);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(caller)]);
    }

    #[test]
    fn callvalue_opcode() {
        let mut host = TestHost::default();
        let message = Message { value: Word::from(0xbeef), ..test_message() };
        let interpreter = run_with_host_message([op::CALLVALUE, op::STOP], &mut host, message);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);
    }

    #[test]
    fn calldataload_opcode() {
        let input = Bytes::from(Vec::from([1_u8, 2, 3]));
        let mut host = TestHost::default();
        let message = Message { input, ..test_message() };

        let interpreter = run_with_host_message(
            [op::PUSH0, op::CALLDATALOAD, op::STOP],
            &mut host,
            message.clone(),
        );
        let mut expected = [0_u8; 32];
        expected[..3].copy_from_slice(&[1, 2, 3]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run_with_host_message(
            [op::PUSH1, 0x20, op::CALLDATALOAD, op::STOP],
            &mut host,
            message,
        );
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn calldatasize_opcode() {
        let input = Bytes::from(Vec::from([1_u8, 2, 3, 4]));
        let mut host = TestHost::default();
        let message = Message { input, ..test_message() };
        let interpreter = run_with_host_message([op::CALLDATASIZE, op::STOP], &mut host, message);
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

        let interpreter = run_with_host_message(code, &mut host, message.clone());
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run_with_host_message(
            stack_code([Word::MAX, Word::MAX, Word::from(0)], op::CALLDATACOPY),
            &mut host,
            message,
        );
        core::assert_matches!(interpreter.err, InstrStop::Stop);
    }

    #[test]
    fn codesize_opcode() {
        let interpreter = run([op::CODESIZE, op::STOP]);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(2)]);

        let interpreter = run([op::PUSH1, 0x00, op::CODESIZE, op::STOP]);
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

        let interpreter = run(code);
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
        let interpreter = run(code);
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
        let interpreter = run_with_host_tx_env([op::GASPRICE, op::STOP], &mut host, tx_env);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0x1234)]);
    }

    #[test]
    fn extcodesize_opcode() {
        let mut host = TestHost { code_size: 0x42, ..TestHost::default() };
        let interpreter = run_with_host([op::PUSH1, 0xbe, op::EXTCODESIZE, op::STOP], &mut host);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0x42)]);
    }

    #[test]
    fn extcodehash_opcode() {
        let hash = B256::with_last_byte(0x77);
        let mut host = TestHost { code_hash: hash, ..TestHost::default() };
        let interpreter = run_with_host_and_spec(
            [op::PUSH1, 0xbe, op::EXTCODEHASH, op::STOP],
            &mut host,
            SpecId::CONSTANTINOPLE,
        );
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(hash)]);

        let interpreter = run_with_host([op::PUSH1, 0xbe, op::EXTCODEHASH, op::STOP], &mut host);
        core::assert_matches!(interpreter.err, InstrStop::NotActivated);
    }
}
