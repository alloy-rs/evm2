use super::utils::{as_usize, as_usize_saturated};
use crate::interpreter::{InstrStop, Result, SpecId, Word, memory::resize_memory};
use alloy_primitives::{B256, keccak256};
use evm2_macros::instruction;

fn address_to_word(address: alloy_primitives::Address) -> Word {
    address.into_word().into()
}

fn b256_to_word(value: B256) -> Word {
    Word::from_be_bytes(value.0)
}

fn check_spec(spec: SpecId, min: SpecId) -> Result {
    if !spec.enables(min) {
        return Err(InstrStop::NotActivated);
    }
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn keccak256_instr(cx: _, [offset, len]: [Word]) -> Result<out> {
    let len = as_usize(*len)?;
    let hash = if len == 0 {
        keccak256([])
    } else {
        let offset = as_usize(*offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
        keccak256(cx.state.memory.slice(offset, len)?)
    };
    *out = Word::from_be_bytes(hash.0);
}

#[instruction]
pub(in crate::interpreter) fn address(cx: _) -> out {
    *out = address_to_word(cx.state.tx.address);
}

#[instruction]
pub(in crate::interpreter) fn origin(cx: _) -> out {
    *out = address_to_word(cx.state.tx.origin);
}

#[instruction]
pub(in crate::interpreter) fn caller(cx: _) -> out {
    *out = address_to_word(cx.state.tx.caller);
}

#[instruction]
pub(in crate::interpreter) fn callvalue(cx: _) -> out {
    *out = cx.state.tx.call_value;
}

#[instruction]
pub(in crate::interpreter) fn calldataload(cx: _, [offset]: [Word]) -> out {
    let offset = as_usize_saturated(*offset);
    let input = cx.state.tx.calldata.as_ref();
    let mut word = B256::ZERO;
    if offset < input.len() {
        let len = 32.min(input.len() - offset);
        word[..len].copy_from_slice(&input[offset..offset + len]);
    }
    *out = b256_to_word(word);
}

#[instruction]
pub(in crate::interpreter) fn calldatasize(cx: _) -> out {
    *out = Word::from(cx.state.tx.calldata.len());
}

#[instruction]
pub(in crate::interpreter) fn calldatacopy(
    cx: _,
    [memory_offset, data_offset, len]: [Word],
) -> Result {
    let len = as_usize(*len)?;
    if len == 0 {
        return Ok(());
    }
    let memory_offset = as_usize(*memory_offset)?;
    let data_offset = as_usize_saturated(*data_offset);
    resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, data_offset, len, &cx.state.tx.calldata)
}

#[instruction]
pub(in crate::interpreter) fn codesize(cx: _) -> out {
    *out = Word::from(cx.state.bytecode.len());
}

#[instruction]
pub(in crate::interpreter) fn codecopy(cx: _, [memory_offset, code_offset, len]: [Word]) -> Result {
    let len = as_usize(*len)?;
    if len == 0 {
        return Ok(());
    }
    let memory_offset = as_usize(*memory_offset)?;
    let code_offset = as_usize_saturated(*code_offset);
    resize_memory(cx.gas, cx.state.memory, memory_offset, len)?;
    cx.state.memory.set_data(memory_offset, code_offset, len, cx.state.bytecode.as_slice())
}

#[instruction]
pub(in crate::interpreter) fn gasprice(cx: _) -> out {
    *out = Word::from(cx.state.tx.gas_price);
}

#[instruction]
pub(in crate::interpreter) fn chainid(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::ISTANBUL)?;
    *out = Word::from(cx.state.tx.chain_id);
}

#[instruction]
pub(in crate::interpreter) fn blobhash(cx: _, [index]: [Word]) -> Result<out> {
    check_spec(cx.state.spec, SpecId::CANCUN)?;
    let index = as_usize_saturated(*index);
    *out = cx.state.tx.blob_hashes.get(index).copied().map(b256_to_word).unwrap_or_default();
}

#[instruction]
pub(in crate::interpreter) fn gas(cx: _) -> out {
    *out = Word::from(cx.gas.remaining());
}

#[cfg(test)]
mod tests {
    use crate::{
        env::TxEnv,
        interpreter::{
            InstrStop, SpecId, Word,
            instructions::tests::{
                TestHost, push, run, run_stack, run_with_host, run_with_host_and_spec,
            },
            op,
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256, Bytes, keccak256};

    fn address_word(address: Address) -> Word {
        address.into_word().into()
    }

    fn test_host(tx: TxEnv) -> TestHost {
        TestHost { tx, ..TestHost::default() }
    }

    fn stack_code<const N: usize>(inputs: [Word; N], opcode: u8) -> Vec<u8> {
        let mut code = Vec::new();
        for input in inputs {
            push(&mut code, input);
        }
        code.extend([opcode, op::STOP]);
        code
    }

    #[test]
    fn keccak256_opcode() {
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, 0);
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([]).0)]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(0x80));
        code.push(op::MSTORE8);
        push(&mut code, 0);
        push(&mut code, Word::from(1));
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([0x80]).0)]);

        let interpreter = run_stack([Word::MAX, Word::from(0)], op::KECCAK256);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(keccak256([]).0)]);

        let interpreter = run_stack([Word::MAX, Word::from(1)], op::KECCAK256);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }

    #[test]
    fn address_opcode() {
        let address = Address::from([0x11; 20]);
        let mut host = test_host(TxEnv { address, ..TxEnv::default() });
        let interpreter = run_with_host([op::ADDRESS, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [address_word(address)]);
    }

    #[test]
    fn origin_opcode() {
        let origin = Address::from([0x22; 20]);
        let mut host = test_host(TxEnv { origin, ..TxEnv::default() });
        let interpreter = run_with_host([op::ORIGIN, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [address_word(origin)]);
    }

    #[test]
    fn caller_opcode() {
        let caller = Address::from([0x33; 20]);
        let mut host = test_host(TxEnv { caller, ..TxEnv::default() });
        let interpreter = run_with_host([op::CALLER, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [address_word(caller)]);
    }

    #[test]
    fn callvalue_opcode() {
        let mut host = test_host(TxEnv { call_value: Word::from(0xbeef), ..TxEnv::default() });
        let interpreter = run_with_host([op::CALLVALUE, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);
    }

    #[test]
    fn calldataload_opcode() {
        let calldata = Bytes::from(Vec::from([1_u8, 2, 3]));
        let mut host = test_host(TxEnv { calldata, ..TxEnv::default() });

        let interpreter = run_with_host([op::PUSH0, op::CALLDATALOAD, op::STOP], &mut host);
        let mut expected = [0_u8; 32];
        expected[..3].copy_from_slice(&[1, 2, 3]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run_with_host([op::PUSH1, 0x20, op::CALLDATALOAD, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn calldatasize_opcode() {
        let calldata = Bytes::from(Vec::from([1_u8, 2, 3, 4]));
        let mut host = test_host(TxEnv { calldata, ..TxEnv::default() });
        let interpreter = run_with_host([op::CALLDATASIZE, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(4)]);
    }

    #[test]
    fn calldatacopy_opcode() {
        let calldata = Bytes::from(Vec::from([0xaa_u8, 0xbb, 0xcc]));
        let mut host = test_host(TxEnv { calldata, ..TxEnv::default() });
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, 1);
        push(&mut code, 2);
        code.push(op::CALLDATACOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run_with_host(code, &mut host);
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let interpreter = run_with_host(
            stack_code([Word::MAX, Word::MAX, Word::from(0)], op::CALLDATACOPY),
            &mut host,
        );
        assert!(matches!(interpreter.err, InstrStop::Stop));
    }

    #[test]
    fn codesize_opcode() {
        let interpreter = run([op::CODESIZE, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(2)]);

        let interpreter = run([op::PUSH1, 0x00, op::CODESIZE, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(0), Word::from(4)]);
    }

    #[test]
    fn codecopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(5));
        push(&mut code, Word::from(2));
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interpreter = run(code);
        let mut expected = [0u8; 32];
        expected[..2].copy_from_slice(&[op::CODECOPY, op::PUSH0]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(expected)]);

        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, Word::from(usize::MAX));
        push(&mut code, Word::from(1));
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);
        let interpreter = run(code);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);

        let interpreter = run_stack([Word::MAX, Word::MAX, Word::from(0)], op::CODECOPY);
        assert!(matches!(interpreter.err, InstrStop::Stop));

        let interpreter = run_stack([Word::MAX, Word::from(0), Word::from(1)], op::CODECOPY);
        assert!(matches!(interpreter.err, InstrStop::InvalidOperandOOG));
    }

    #[test]
    fn gasprice_opcode() {
        let mut host = test_host(TxEnv { gas_price: 0x1234, ..TxEnv::default() });
        let interpreter = run_with_host([op::GASPRICE, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(0x1234)]);
    }

    #[test]
    fn chainid_opcode() {
        let mut host = test_host(TxEnv { chain_id: 1, ..TxEnv::default() });
        let interpreter =
            run_with_host_and_spec([op::CHAINID, op::STOP], &mut host, SpecId::ISTANBUL);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(1)]);
    }

    #[test]
    fn blobhash_opcode() {
        let hash = B256::with_last_byte(0x42);
        let mut host = test_host(TxEnv { blob_hashes: Vec::from([hash]), ..TxEnv::default() });

        let interpreter =
            run_with_host_and_spec([op::PUSH0, op::BLOBHASH, op::STOP], &mut host, SpecId::CANCUN);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from_be_bytes(hash.0)]);

        let interpreter = run_with_host_and_spec(
            [op::PUSH1, 0x01, op::BLOBHASH, op::STOP],
            &mut host,
            SpecId::CANCUN,
        );
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn gas_opcode() {
        let interpreter = run([op::GAS, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 1);
        assert!(interpreter.stack()[0] < Word::from(10_000));

        let interpreter = run([op::GAS, op::GAS, op::STOP]);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack().len(), 2);
        assert!(interpreter.stack()[1] < interpreter.stack()[0]);
    }
}
