//! evm2 JIT interpreter dispatch helpers.

use crate::runtime::{InterpretReason, JitBackend, LookupDecision, LookupRequest, RuntimeCacheKey};
use alloy_primitives::{Bytes, keccak256};
use evm2::{
    EvmTypes, ExecutionConfig, InterpreterRunner,
    interpreter::{InstrStop, Interpreter},
};

/// External interpreter runner backed by [`JitBackend`].
#[derive(Clone, Debug)]
pub struct JitInterpreterRunner {
    backend: JitBackend,
}

impl JitInterpreterRunner {
    /// Creates a runner from a JIT backend.
    #[inline]
    pub const fn new(backend: JitBackend) -> Self {
        Self { backend }
    }

    /// Returns the JIT backend.
    #[inline]
    pub const fn backend(&self) -> &JitBackend {
        &self.backend
    }
}

impl<T: EvmTypes> InterpreterRunner<T> for JitInterpreterRunner {
    #[inline]
    fn run(
        &self,
        config: &ExecutionConfig<T>,
        interpreter: &mut Interpreter<'_, T>,
        host: &mut T::Host,
    ) -> Option<InstrStop> {
        match run_interpreter(&self.backend, config, interpreter, host) {
            JitRunResult::Finished(stop) => Some(stop),
            JitRunResult::Interpret(_) => None,
        }
    }
}

/// Result of trying to execute an evm2 interpreter frame through JIT code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitRunResult {
    /// No compiled program was available, so the caller should run the evm2 interpreter.
    Interpret(InterpretReason),
    /// Compiled execution finished the frame.
    Finished(InstrStop),
}

/// Attempts to execute `interpreter` through a compiled evm2-compatible program.
///
/// On [`JitRunResult::Interpret`], callers should fall back to
/// [`Interpreter::run`] with the same `config` and `host`.
#[inline]
pub fn run_interpreter<T: EvmTypes>(
    backend: &JitBackend,
    config: &ExecutionConfig<T>,
    interpreter: &mut Interpreter<'_, T>,
    host: &mut T::Host,
) -> JitRunResult {
    let bytecode = interpreter.bytecode();
    let code = bytecode.as_slice();
    let decision = backend.lookup(LookupRequest {
        key: RuntimeCacheKey { code_hash: keccak256(code), spec_id: config.base_spec_id() },
        code: Bytes::copy_from_slice(code),
    });

    let program = match decision {
        LookupDecision::Compiled(program) => program,
        LookupDecision::Interpret(reason) => return JitRunResult::Interpret(reason),
    };

    interpreter.prepare_jit_run(config, host);
    JitRunResult::Finished(unsafe {
        program.evm2_func::<T>().call_with_interpreter(interpreter, host)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeConfig;
    use alloy_primitives::Address;
    use evm2::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmConfigSelector, Precompiles, SpecId,
        bytecode::Bytecode,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::{AccountInfo, EmptyDB, InMemoryDB},
        interpreter::{Message, op},
    };

    const BYTECODE_RET42: &[u8] =
        &[op::PUSH1, 0x42, op::PUSH0, op::MSTORE, op::PUSH1, 0x20, op::PUSH0, op::RETURN];

    fn blocking_backend() -> JitBackend {
        JitBackend::new(RuntimeConfig { enabled: true, blocking: true, ..RuntimeConfig::default() })
            .unwrap()
    }

    fn push20(code: &mut Vec<u8>, address: Address) {
        code.push(op::PUSH20);
        code.extend_from_slice(address.as_slice());
    }

    #[test]
    fn disabled_backend_falls_back_to_interpreter() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        assert_eq!(
            run_interpreter(&JitBackend::disabled(), &config, &mut interpreter, &mut host),
            JitRunResult::Interpret(InterpretReason::Disabled),
        );
    }

    #[test]
    fn compiled_call_executes_recursive_evm2_message() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let target = Address::from([0x22; 20]);
        let caller = Address::from([0x11; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default());
        database.insert_account_info(
            &target,
            AccountInfo::default()
                .with_code(Bytecode::new_legacy(Bytes::copy_from_slice(BYTECODE_RET42))),
        );
        let backend = blocking_backend();
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, destination: caller, ..Message::default() };
        let mut code = vec![op::PUSH1, 0x20, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0];
        push20(&mut code, target);
        code.extend([
            op::PUSH2,
            0x27,
            0x10,
            op::CALL,
            op::PUSH0,
            op::MLOAD,
            op::PUSH0,
            op::MSTORE,
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::RETURN,
        ]);
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(code.into()),
            &tx_env,
            &message,
            false,
        );

        assert_eq!(
            run_interpreter(&backend, &config, &mut interpreter, &mut host),
            JitRunResult::Finished(InstrStop::Return),
        );
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 0x42);
    }

    #[test]
    fn compiled_create_executes_recursive_evm2_message() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let creator = Address::from([0x33; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&creator, AccountInfo::default());
        let backend = blocking_backend();
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, destination: creator, ..Message::default() };
        let code = [
            op::PUSH10,
            0x60,
            0x00,
            0x60,
            0x00,
            0x53,
            0x60,
            0x01,
            0x60,
            0x00,
            0xf3,
            op::PUSH0,
            op::MSTORE,
            op::PUSH1,
            0x0a,
            op::PUSH1,
            0x16,
            op::PUSH0,
            op::CREATE,
            op::DUP1,
            op::EXTCODESIZE,
            op::PUSH0,
            op::MSTORE,
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::RETURN,
        ];
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
            &tx_env,
            &message,
            false,
        );

        assert_eq!(
            run_interpreter(&backend, &config, &mut interpreter, &mut host),
            JitRunResult::Finished(InstrStop::Return),
        );
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 1);
    }
}
