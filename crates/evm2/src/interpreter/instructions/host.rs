use super::utils::{as_usize, check_spec};
use crate::interpreter::{
    GasId, GasParams, InstrStop, Result, SpecId, Word, memory::resize_memory, table::InstructionCx,
};
use alloy_primitives::{B256, Bytes, Log, LogData};
use evm2_macros::instruction;

const fn require_non_staticcall(cx: &InstructionCx<'_, '_, '_>) -> Result {
    if cx.state.is_static {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    Ok(())
}

#[instruction]
pub(in crate::interpreter) fn sload(cx: _, [index]: [Word]) -> out {
    *out = cx.state.host.sload(index);
}

#[instruction(raw)]
pub(in crate::interpreter) fn sstore(cx: _) -> Result {
    require_non_staticcall(&cx)?;
    let [index, value] = stack.popn()?;
    let gas_params = GasParams::new_spec(cx.state.spec);
    if cx.state.spec.enables(SpecId::ISTANBUL)
        && cx.gas.remaining() <= gas_params.get(GasId::CallStipend)
    {
        return Err(InstrStop::ReentrancySentryOOG);
    }
    cx.gas.spend(gas_params.get(GasId::SstoreStatic))?;
    cx.state.host.sstore(index, value);
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn tload(cx: _) -> Result {
    check_spec(cx.state.spec, SpecId::CANCUN)?;
    let ([], index) = stack.popn_top()?;
    *index = cx.state.host.tload(*index);
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn tstore(cx: _) -> Result {
    check_spec(cx.state.spec, SpecId::CANCUN)?;
    require_non_staticcall(&cx)?;
    let [index, value] = stack.popn()?;
    cx.state.host.tstore(index, value);
    Ok(())
}

#[instruction(raw)]
pub(in crate::interpreter) fn log<const N: usize>(cx: _) -> Result {
    require_non_staticcall(&cx)?;
    let [offset, len] = stack.popn()?;
    let len = as_usize(len)?;
    let gas_params = GasParams::new_spec(cx.state.spec);
    cx.gas.spend(gas_params.log_cost(N as u8, len))?;

    let data = if len == 0 {
        Bytes::new()
    } else {
        let offset = as_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory, offset, len)?;
        Bytes::copy_from_slice(cx.state.memory.slice(offset, len)?)
    };

    let topics =
        stack.popn::<N>()?.into_iter().map(|topic| B256::from(topic.to_be_bytes::<32>())).collect();
    cx.state.host.log(Log {
        address: cx.state.message.destination,
        data: LogData::new(topics, data).expect("LOG opcodes cannot emit more than 4 topics"),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrStop, Message, MessageKind, SpecId, Word,
        instructions::tests::{
            TestHost, push, run_with_host, run_with_host_and_spec, run_with_host_and_spec_config,
            run_with_host_message,
        },
        op,
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256, Bytes};

    #[test]
    fn sload_opcode() {
        let mut host = TestHost::default();
        host.storage.insert(Word::from(1), Word::from(0xbeef));

        let mut code = Vec::new();
        push(&mut code, 1);
        code.extend([op::SLOAD, op::STOP]);
        let interpreter = run_with_host(code, &mut host);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);

        let mut code = Vec::new();
        push(&mut code, 2);
        code.extend([op::SLOAD, op::STOP]);
        let interpreter = run_with_host(code, &mut host);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn sstore_opcode() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.push(op::SSTORE);
        push(&mut code, 1);
        code.extend([op::SLOAD, op::STOP]);

        let interpreter = run_with_host(code, &mut host);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);
        assert_eq!(host.storage.get(&Word::from(1)), Some(&Word::from(0xbeef)));
    }

    #[test]
    fn sstore_staticcall_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);

        let interpreter =
            run_with_host_and_spec_config(code, &mut host, SpecId::HOMESTEAD, true, 10_000);
        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef), Word::from(1)]);
        assert_eq!(host.storage.get(&Word::from(1)), None);
    }

    #[test]
    fn sstore_staticcall_message_kind_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);
        let message =
            Message { kind: MessageKind::StaticCall, gas_limit: 10_000, ..Default::default() };

        let interpreter = run_with_host_message(code, &mut host, message);

        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef), Word::from(1)]);
        assert_eq!(host.storage.get(&Word::from(1)), None);
    }

    #[test]
    fn sstore_stipend_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);

        let interpreter =
            run_with_host_and_spec_config(code, &mut host, SpecId::ISTANBUL, false, 2306);
        core::assert_matches!(interpreter.err, InstrStop::ReentrancySentryOOG);
        assert_eq!(host.storage.get(&Word::from(1)), None);
    }

    #[test]
    fn sstore_static_gas() {
        let mut host = TestHost::default();
        let interpreter = run_with_host_and_spec_config(
            [op::PUSH1, 0, op::PUSH1, 0, op::SSTORE, op::STOP],
            &mut host,
            SpecId::FRONTIER,
            false,
            6000,
        );
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.gas_remaining(), 994);
    }

    #[test]
    fn tload_opcode() {
        let mut host = TestHost::default();
        host.transient_storage.insert(Word::from(1), Word::from(0xcafe));

        let mut code = Vec::new();
        push(&mut code, 1);
        code.extend([op::TLOAD, op::STOP]);
        let interpreter = run_with_host_and_spec(code, &mut host, SpecId::CANCUN);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xcafe)]);

        let interpreter = run_with_host([op::PUSH0, op::TLOAD, op::STOP], &mut host);
        core::assert_matches!(interpreter.err, InstrStop::NotActivated);
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn tstore_opcode() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xcafe);
        push(&mut code, 1);
        code.push(op::TSTORE);
        push(&mut code, 1);
        code.extend([op::TLOAD, op::STOP]);

        let interpreter = run_with_host_and_spec(code, &mut host, SpecId::CANCUN);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xcafe)]);
        assert_eq!(host.transient_storage.get(&Word::from(1)), Some(&Word::from(0xcafe)));

        let interpreter = run_with_host([op::PUSH0, op::PUSH0, op::TSTORE, op::STOP], &mut host);
        core::assert_matches!(interpreter.err, InstrStop::NotActivated);
        assert_eq!(interpreter.stack(), [0, 0]);
    }

    #[test]
    fn tstore_staticcall_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xcafe);
        push(&mut code, 1);
        code.extend([op::TSTORE, op::STOP]);

        let interpreter =
            run_with_host_and_spec_config(code, &mut host, SpecId::CANCUN, true, 10_000);
        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert_eq!(interpreter.stack(), [Word::from(0xcafe), Word::from(1)]);
        assert_eq!(host.transient_storage.get(&Word::from(1)), None);
    }

    fn log_code<const N: usize>(offset: usize, len: usize, topics: [Word; N]) -> Vec<u8> {
        let mut code = Vec::new();
        push(&mut code, Word::from(0xbeef));
        push(&mut code, 0);
        code.push(op::MSTORE);
        for topic in topics.into_iter().rev() {
            push(&mut code, topic);
        }
        push(&mut code, len);
        push(&mut code, offset);
        code.push(op::LOG0 + N as u8);
        code.push(op::STOP);
        code
    }

    #[test]
    fn log0_opcode() {
        let mut host = TestHost::default();
        let address = Address::from([0x11; 20]);
        let message = crate::interpreter::Message {
            destination: address,
            gas_limit: 10_000,
            ..Default::default()
        };
        let interpreter = run_with_host_message(log_code(30, 2, []), &mut host, message);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert!(interpreter.stack().is_empty());
        assert_eq!(host.logs.len(), 1);
        assert_eq!(host.logs[0].address, address);
        assert!(host.logs[0].topics().is_empty());
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe, 0xef]));
    }

    #[test]
    fn log1_opcode() {
        let mut host = TestHost::default();
        let interpreter = run_with_host(log_code(30, 2, [Word::from(1)]), &mut host);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics(), &[B256::from(Word::from(1).to_be_bytes::<32>())]);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe, 0xef]));
    }

    #[test]
    fn log2_opcode() {
        let mut host = TestHost::default();
        let interpreter = run_with_host(log_code(30, 0, [Word::from(1), Word::from(2)]), &mut host);
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(
            host.logs[0].topics(),
            &[
                B256::from(Word::from(1).to_be_bytes::<32>()),
                B256::from(Word::from(2).to_be_bytes::<32>()),
            ]
        );
        assert!(host.logs[0].data.data.is_empty());
    }

    #[test]
    fn log3_opcode() {
        let mut host = TestHost::default();
        let interpreter = run_with_host(
            log_code(30, 1, [Word::from(1), Word::from(2), Word::from(3)]),
            &mut host,
        );
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics().len(), 3);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe]));
    }

    #[test]
    fn log4_opcode() {
        let mut host = TestHost::default();
        let interpreter = run_with_host(
            log_code(30, 1, [Word::from(1), Word::from(2), Word::from(3), Word::from(4)]),
            &mut host,
        );
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics().len(), 4);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe]));
    }

    #[test]
    fn log_staticcall_check() {
        let mut host = TestHost::default();
        let interpreter = run_with_host_and_spec_config(
            log_code(30, 2, []),
            &mut host,
            SpecId::HOMESTEAD,
            true,
            10_000,
        );
        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert!(host.logs.is_empty());
    }

    #[test]
    fn log_memory_oog() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 1);
        push(&mut code, Word::MAX);
        code.extend([op::LOG0, op::STOP]);
        let interpreter = run_with_host(code, &mut host);
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
        assert!(host.logs.is_empty());
    }
}
