use super::utils::check_spec;
use crate::interpreter::{GasId, GasParams, InstrStop, Result, SpecId, Word, table::InstructionCx};
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

#[cfg(test)]
mod tests {
    use crate::interpreter::{
        InstrStop, SpecId, Word,
        instructions::tests::{
            TestHost, push, run_with_host, run_with_host_and_spec, run_with_host_and_spec_config,
        },
        op,
    };
    use alloc::vec::Vec;

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
}
