use super::utils::as_usize;
use crate::{
    EvmTypes, SpecId,
    interpreter::{
        GasId, Host, InstrStop, InstructionCx, Result, StackMut, Word, memory::resize_memory,
    },
};
use alloy_primitives::{B256, Bytes, Log, LogData};
use evm2_macros::instruction;

#[inline]
fn require_non_staticcall<T: EvmTypes>(cx: &InstructionCx<'_, '_, T>) -> Result {
    if cx.state.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    Ok(())
}

#[instruction]
pub(crate) fn sload(cx: _, [key]: [Word]) -> Result<out> {
    let load = cx.state.host.sload(cx.state.message().destination, key);
    if load.is_cold {
        cx.gas.spend(cx.state.gas_params().get(GasId::ColdStorageAdditionalCost).into())?;
    }
    *out = load.value;
}

#[instruction(raw)]
pub(crate) fn sstore(cx: _) -> Result {
    require_non_staticcall(&cx)?;
    let [key, value] = stack.popn()?;
    let gas_params = cx.state.gas_params();
    if cx.state.spec.enables(SpecId::ISTANBUL)
        && cx.gas.remaining() <= gas_params.get(GasId::CallStipend).into()
    {
        return Err(InstrStop::ReentrancySentryOOG);
    }
    let load = cx.state.host.sload(cx.state.message().destination, key);
    let old_value = load.value;
    cx.gas.spend(gas_params.get(GasId::SstoreStatic).into())?;
    if load.is_cold {
        cx.gas.spend(gas_params.get(GasId::ColdStorageCost).into())?;
    }
    if old_value == value {
        // No-op stores only pay the load/static cost after Istanbul.
    } else if old_value.is_zero() {
        cx.gas.spend(gas_params.get(GasId::SstoreSetWithoutLoadCost).into())?;
    } else if value.is_zero() {
        cx.gas.record_refund(gas_params.get(GasId::SstoreClearingSlotRefund) as i64);
        cx.gas.spend(gas_params.get(GasId::SstoreResetWithoutColdLoadCost).into())?;
    } else {
        cx.gas.spend(gas_params.get(GasId::SstoreResetWithoutColdLoadCost).into())?;
    }
    cx.state.host.sstore(cx.state.message().destination, key, value);
    Ok(())
}

#[instruction(raw)]
pub(crate) fn tload(cx: _) -> Result {
    let ([], key) = stack.popn_top()?;
    *key = cx.state.host.tload(cx.state.message().destination, *key);
    Ok(())
}

#[instruction(raw)]
pub(crate) fn tstore(cx: _) -> Result {
    require_non_staticcall(&cx)?;
    let [key, value] = stack.popn()?;
    cx.state.host.tstore(cx.state.message().destination, key, value);
    Ok(())
}

#[instruction(raw)]
pub(crate) fn log<const N: usize>(cx: _) -> Result {
    log_common(cx, stack, N)
}

#[inline(never)]
fn log_common<T: EvmTypes>(
    cx: InstructionCx<'_, '_, T>,
    mut stack: StackMut<'_>,
    n: usize,
) -> Result {
    require_non_staticcall(&cx)?;
    let [offset, len] = stack.popn()?;
    let len = as_usize(len)?;
    cx.gas.spend(cx.state.gas_params().log_cost(n as u8, len))?;

    let data = if len == 0 {
        Bytes::new()
    } else {
        let offset = as_usize(offset)?;
        resize_memory(cx.gas, cx.state.memory(), offset, len)?;
        Bytes::copy_from_slice(cx.state.memory().slice(offset, len))
    };

    let topics = stack.popn_dyn(n)?.map(|topic| B256::from(topic.to_be_bytes::<32>())).collect();
    cx.state.host.log(Log {
        address: cx.state.message().destination,
        data: LogData::new(topics, data).expect("LOG opcodes cannot emit more than 4 topics"),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        SpecId,
        interpreter::{
            InstrStop, Message, MessageKind, Word,
            instructions::tests::{RunConfig, TestHost, push, run},
            op,
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256, Bytes};

    #[test]
    fn sload_opcode() {
        let mut host = TestHost::default();
        host.storage.insert((Address::ZERO, Word::from(1)), Word::from(0xbeef));

        let mut code = Vec::new();
        push(&mut code, 1);
        code.extend([op::SLOAD, op::STOP]);
        let interpreter = run(RunConfig::new(code).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);

        let mut code = Vec::new();
        push(&mut code, 2);
        code.extend([op::SLOAD, op::STOP]);
        let interpreter = run(RunConfig::new(code).host(&mut host));
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

        let interpreter = run(RunConfig::new(code).host(&mut host).gas_limit(30_000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef)]);
        assert_eq!(host.storage.get(&(Address::ZERO, Word::from(1))), Some(&Word::from(0xbeef)));
    }

    #[test]
    fn sstore_staticcall_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);

        let interpreter = run(RunConfig::new(code).host(&mut host).staticcall());
        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef), Word::from(1)]);
        assert_eq!(host.storage.get(&(Address::ZERO, Word::from(1))), None);
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

        let interpreter = run(RunConfig::new(code).host(&mut host).message(message));

        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert_eq!(interpreter.stack(), [Word::from(0xbeef), Word::from(1)]);
        assert_eq!(host.storage.get(&(Address::ZERO, Word::from(1))), None);
    }

    #[test]
    fn sstore_stipend_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);

        let interpreter =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::ISTANBUL).gas_limit(2306));
        core::assert_matches!(interpreter.err, InstrStop::ReentrancySentryOOG);
        assert_eq!(host.storage.get(&(Address::ZERO, Word::from(1))), None);
    }

    #[test]
    fn sstore_static_gas() {
        let mut host = TestHost::default();
        let interpreter = run(RunConfig::new([op::PUSH1, 0, op::PUSH1, 0, op::SSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::FRONTIER)
            .gas_limit(6000));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.gas_remaining(), 994);
    }

    #[test]
    fn sstore_noop_uses_warm_load_gas() {
        let mut host = TestHost::default();
        let interpreter = run(RunConfig::new([op::PUSH1, 0, op::PUSH1, 0, op::SSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .gas_limit(3000));

        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.gas_remaining(), 2894);
    }

    #[test]
    fn tload_opcode() {
        let mut host = TestHost::default();
        host.transient_storage.insert((Address::ZERO, Word::from(1)), Word::from(0xcafe));

        let mut code = Vec::new();
        push(&mut code, 1);
        code.extend([op::TLOAD, op::STOP]);
        let interpreter = run(RunConfig::new(code).host(&mut host).spec(SpecId::CANCUN));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xcafe)]);

        let interpreter = run(RunConfig::new([op::PUSH1, 0, op::TLOAD, op::STOP])
            .host(&mut host)
            .spec(SpecId::SHANGHAI));
        core::assert_matches!(interpreter.err, InstrStop::OpcodeNotFound);
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

        let interpreter = run(RunConfig::new(code).host(&mut host).spec(SpecId::CANCUN));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(0xcafe)]);
        assert_eq!(
            host.transient_storage.get(&(Address::ZERO, Word::from(1))),
            Some(&Word::from(0xcafe))
        );

        let interpreter = run(RunConfig::new([op::PUSH1, 0, op::PUSH1, 0, op::TSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::SHANGHAI));
        core::assert_matches!(interpreter.err, InstrStop::OpcodeNotFound);
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
            run(RunConfig::new(code).host(&mut host).spec(SpecId::CANCUN).staticcall());
        core::assert_matches!(interpreter.err, InstrStop::StateChangeDuringStaticCall);
        assert_eq!(interpreter.stack(), [Word::from(0xcafe), Word::from(1)]);
        assert_eq!(host.transient_storage.get(&(Address::ZERO, Word::from(1))), None);
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
        let interpreter = run(RunConfig::new(log_code(30, 2, [])).host(&mut host).message(message));
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
        let interpreter = run(RunConfig::new(log_code(30, 2, [Word::from(1)])).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics(), &[B256::from(Word::from(1).to_be_bytes::<32>())]);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe, 0xef]));
    }

    #[test]
    fn log2_opcode() {
        let mut host = TestHost::default();
        let interpreter =
            run(RunConfig::new(log_code(30, 0, [Word::from(1), Word::from(2)])).host(&mut host));
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
        let interpreter =
            run(RunConfig::new(log_code(30, 1, [Word::from(1), Word::from(2), Word::from(3)]))
                .host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics().len(), 3);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe]));
    }

    #[test]
    fn log4_opcode() {
        let mut host = TestHost::default();
        let interpreter = run(RunConfig::new(log_code(
            30,
            1,
            [Word::from(1), Word::from(2), Word::from(3), Word::from(4)],
        ))
        .host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics().len(), 4);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe]));
    }

    #[test]
    fn log_staticcall_check() {
        let mut host = TestHost::default();
        let interpreter = run(RunConfig::new(log_code(30, 2, [])).host(&mut host).staticcall());
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
        let interpreter = run(RunConfig::new(code).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::InvalidOperandOOG);
        assert!(host.logs.is_empty());
    }
}
