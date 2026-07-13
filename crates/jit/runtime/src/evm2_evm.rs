//! evm2 JIT interpreter dispatch helpers.

use crate::runtime::{JitBackend, LookupDecision, LookupRequest, RuntimeCacheKey};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, InterpreterRunner,
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

impl InterpreterRunner<BaseEvmTypes> for JitInterpreterRunner {
    #[inline]
    fn run<'frame, 'host>(
        &self,
        config: &ExecutionConfig<BaseEvmTypes>,
        interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        host: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        run_interpreter(&self.backend, config, interpreter, host)
    }
}

/// Attempts to execute `interpreter` through a compiled evm2-compatible program.
///
/// Returns `None` when no compiled program is available, leaving the caller to
/// run the same frame through the evm2 interpreter.
#[inline]
pub fn run_interpreter<'frame, 'host>(
    backend: &JitBackend,
    config: &ExecutionConfig<BaseEvmTypes>,
    interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
    host: &mut Evm<'host, BaseEvmTypes>,
) -> Option<InstrStop> {
    let code_hash = interpreter.original_bytecode_hash();
    let code = interpreter.original_bytecode();
    let decision = backend.lookup(LookupRequest {
        key: RuntimeCacheKey { code_hash, spec_id: config.base_spec_id() },
        code,
    });

    let program = match decision {
        LookupDecision::Compiled(program) => program,
        LookupDecision::Unavailable(_) => return None,
    };

    interpreter.prepare_run(config.base_spec_id(), config.version(), host);
    Some(unsafe { program.func.call_with_interpreter(interpreter) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "llvm")]
    use crate::runtime::RuntimeConfig;
    #[cfg(feature = "llvm")]
    use alloy_primitives::Address;
    use alloy_primitives::Bytes;
    use evm2::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmConfigSelector, Precompiles, SpecId,
        bytecode::Bytecode,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::EmptyDB,
        interpreter::{Message, op},
    };
    #[cfg(feature = "llvm")]
    use evm2::{
        evm::{AccountInfo, InMemoryDB},
        interpreter::Word,
    };

    #[cfg(feature = "llvm")]
    const BYTECODE_RET42: &[u8] =
        &[op::PUSH1, 0x42, op::PUSH0, op::MSTORE, op::PUSH1, 0x20, op::PUSH0, op::RETURN];

    #[cfg(feature = "llvm")]
    fn blocking_backend() -> JitBackend {
        JitBackend::new(RuntimeConfig { enabled: true, blocking: true, ..RuntimeConfig::default() })
            .unwrap()
    }

    #[cfg(feature = "llvm")]
    fn push20(code: &mut Vec<u8>, address: Address) {
        code.push(op::PUSH20);
        code.extend_from_slice(address.as_slice());
    }

    #[cfg(feature = "llvm")]
    fn staticcall_gas_leaf_code() -> Vec<u8> {
        let mut code = Vec::with_capacity(60_005);
        for _ in 0..12_000 {
            code.extend([op::PUSH1, 1, op::PUSH1, 1, op::ADD]);
        }
        code.extend([op::PUSH1, 1, op::PUSH1, 0, op::SSTORE]);
        code
    }

    #[cfg(feature = "llvm")]
    fn staticcall_loop_code(leaf: Address) -> Vec<u8> {
        let mut code = vec![
            op::JUMPDEST,
            op::PUSH1,
            0x32,
            op::PUSH1,
            0x80,
            op::MLOAD,
            op::LT,
            op::ISZERO,
            op::PUSH1,
            0x3e,
            op::JUMPI,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
        ];
        push20(&mut code, leaf);
        code.extend([
            op::PUSH5,
            0x14,
            0x8c,
            0x1c,
            0x22,
            0x80,
            op::STATICCALL,
            op::PUSH1,
            0,
            op::SSTORE,
            op::PUSH1,
            1,
            op::PUSH1,
            0x80,
            op::MLOAD,
            op::ADD,
            op::PUSH1,
            0x80,
            op::MSTORE,
            op::PUSH1,
            0,
            op::JUMP,
            op::JUMPDEST,
            op::PUSH1,
            0x80,
            op::MLOAD,
            op::PUSH1,
            1,
            op::SSTORE,
            op::STOP,
        ]);
        code
    }

    #[cfg(feature = "llvm")]
    fn dynamic_call_outer_code() -> Vec<u8> {
        vec![
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::CALLVALUE,
            op::PUSH1,
            0,
            op::CALLDATALOAD,
            op::GAS,
            op::CALL,
            op::PUSH1,
            0,
            op::SSTORE,
            op::PUSH1,
            1,
            op::PUSH1,
            1,
            op::SSTORE,
            op::STOP,
        ]
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
            None
        );
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_call_executes_message() {
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
        let mut interpreter =
            Interpreter::<BaseEvmTypes>::new(Bytecode::new_legacy(code.into()), &tx_env, &message);

        assert_eq!(
            run_interpreter(&backend, &config, &mut interpreter, &mut host),
            Some(InstrStop::Return),
        );
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 0x42);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_create_executes_message() {
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
        );

        assert_eq!(
            run_interpreter(&backend, &config, &mut interpreter, &mut host),
            Some(InstrStop::Return),
        );
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 1);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_amsterdam_create_checks_balance_before_state_gas() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::AMSTERDAM,
        );
        let creator = Address::from([0x33; 20]);
        let code = [
            op::PUSH0,
            op::PUSH0,
            op::PUSH1,
            2,
            op::CREATE,
            op::PUSH0,
            op::MSTORE,
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::RETURN,
        ];

        let run = |with_jit: bool| {
            let mut database = InMemoryDB::default();
            database
                .insert_account_info(&creator, AccountInfo::default().with_balance(Word::from(1)));
            let backend = blocking_backend();
            let mut host = Evm::<BaseEvmTypes>::new(
                SpecId::AMSTERDAM,
                BlockEnv::default(),
                ethereum_tx_registry(SpecId::AMSTERDAM),
                database,
                Precompiles::base(SpecId::AMSTERDAM),
            );
            host.state_mut().prewarm(&creator);
            if with_jit {
                host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
            }
            let tx_env = TxEnv::default();
            let message =
                Message { gas_limit: 100_000, destination: creator, ..Message::default() };
            let mut interpreter = Interpreter::<BaseEvmTypes>::new(
                Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
                &tx_env,
                &message,
            );
            let stop = if with_jit {
                run_interpreter(&backend, &config, &mut interpreter, &mut host).unwrap()
            } else {
                interpreter.run(&config, &mut host)
            };
            (
                stop,
                interpreter.gas().spent(),
                interpreter.gas().refunded(),
                interpreter.output().to_vec(),
            )
        };

        let interpreter = run(false);
        let jit = run(true);
        assert_eq!(jit, interpreter);
        assert_eq!(jit.0, InstrStop::Return);
        assert_eq!(jit.3, vec![0; 32]);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_staticcall_zeros_child_callvalue() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let caller = Address::from([0x11; 20]);
        let target = Address::with_last_byte(0x68);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default());
        database.insert_account_info(
            &target,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
                op::CALLVALUE,
                op::PUSH0,
                op::MSTORE,
                op::PUSH1,
                0x20,
                op::PUSH0,
                op::RETURN,
            ]))),
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
        let message = Message {
            gas_limit: 1_000_000,
            destination: caller,
            value: Word::from(30),
            ..Message::default()
        };
        let code = vec![
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::PUSH0,
            op::PUSH0,
            op::PUSH1,
            0x68,
            op::PUSH2,
            0x27,
            0x10,
            op::STATICCALL,
            op::POP,
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::RETURN,
        ];
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
            &tx_env,
            &message,
        );

        assert_eq!(
            run_interpreter(&backend, &config, &mut interpreter, &mut host),
            Some(InstrStop::Return),
        );
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 0);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_staticcall_child_callvalue_does_not_transfer() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let caller = Address::from([0x11; 20]);
        let child = Address::with_last_byte(0x68);
        let beneficiary = Address::with_last_byte(0x69);
        let initial_beneficiary_balance = Word::from(7);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::from(100)));
        database.insert_account_info(
            &child,
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[
                op::PUSH0,
                op::PUSH0,
                op::PUSH0,
                op::PUSH0,
                op::CALLVALUE,
                op::PUSH0,
                op::CALLDATALOAD,
                op::GAS,
                op::CALL,
                op::STOP,
            ]))),
        );
        database.insert_account_info(
            &beneficiary,
            AccountInfo::default().with_balance(initial_beneficiary_balance),
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
        let message = Message {
            gas_limit: 1_000_000,
            destination: caller,
            value: Word::from(30),
            ..Message::default()
        };
        let code = vec![
            op::PUSH1,
            0x69,
            op::PUSH0,
            op::MSTORE,
            op::PUSH0,
            op::PUSH0,
            op::PUSH1,
            0x20,
            op::PUSH0,
            op::PUSH1,
            0x68,
            op::PUSH2,
            0x27,
            0x10,
            op::STATICCALL,
            op::STOP,
        ];
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
            &tx_env,
            &message,
        );

        assert_eq!(
            run_interpreter(&backend, &config, &mut interpreter, &mut host),
            Some(InstrStop::Stop),
        );
        let beneficiary_balance = host.read_account_info(&beneficiary).unwrap().unwrap().balance;
        assert_eq!(beneficiary_balance, initial_beneficiary_balance);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_dynamic_call_to_staticcall_loop_matches_interpreter_gas() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let caller = Address::from([0x11; 20]);
        let outer = Address::with_last_byte(0xfd);
        let middle = Address::with_last_byte(0xed);
        let leaf = Address::with_last_byte(0xdd);
        let outer_code = dynamic_call_outer_code();
        let mut input = [0u8; 32];
        input[12..].copy_from_slice(middle.as_slice());
        let message = Message {
            gas_limit: 0xcd79195900 - 21_368,
            destination: outer,
            caller,
            input: Bytes::copy_from_slice(&input),
            value: Word::from(10),
            code_address: outer,
            ..Message::default()
        };
        let tx_env = TxEnv { gas_price: Word::from(10), ..TxEnv::default() };

        let run = |with_jit: bool| {
            let mut database = InMemoryDB::default();
            database
                .insert_account_info(&caller, AccountInfo::default().with_balance(Word::from(100)));
            database
                .insert_account_info(&outer, AccountInfo::default().with_balance(Word::from(10)));
            database.insert_account_storage(&outer, &Word::from(1), &Word::from(1));
            database.insert_account_info(
                &middle,
                AccountInfo::default()
                    .with_code(Bytecode::new_legacy(Bytes::from(staticcall_loop_code(leaf)))),
            );
            database.insert_account_info(
                &leaf,
                AccountInfo::default()
                    .with_code(Bytecode::new_legacy(Bytes::from(staticcall_gas_leaf_code()))),
            );
            let backend = blocking_backend();
            let mut host = Evm::<BaseEvmTypes>::new(
                SpecId::CANCUN,
                BlockEnv::default(),
                ethereum_tx_registry(SpecId::CANCUN),
                database,
                Precompiles::base(SpecId::CANCUN),
            );
            host.state_mut().prewarm(&caller);
            host.state_mut().prewarm(&outer);
            if with_jit {
                host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
            }
            let mut interpreter = Interpreter::<BaseEvmTypes>::new(
                Bytecode::new_legacy(Bytes::copy_from_slice(&outer_code)),
                &tx_env,
                &message,
            );
            let stop = if with_jit {
                run_interpreter(&backend, &config, &mut interpreter, &mut host).unwrap()
            } else {
                interpreter.run(&config, &mut host)
            };
            (stop, interpreter.gas().spent(), interpreter.gas().refunded())
        };

        let interpreter = run(false);
        let jit = run(true);
        assert_eq!(jit, interpreter);
    }
}
