//! evm2 JIT interpreter dispatch helpers.

use crate::runtime::{JitBackend, LookupDecision, LookupRequest, RuntimeCacheKey};
use evm2::{
    BaseEvmTypes, Evm, InterpreterRunner,
    interpreter::{Host, InstrStop, Interpreter},
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
        interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        host: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        run_interpreter(&self.backend, interpreter, host)
    }
}

/// Attempts to execute `interpreter` through a compiled evm2-compatible program.
///
/// Returns `None` when no compiled program is available, leaving the caller to
/// run the same frame through the evm2 interpreter.
#[inline]
pub fn run_interpreter<'frame, 'host>(
    backend: &JitBackend,
    interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
    host: &mut Evm<'host, BaseEvmTypes>,
) -> Option<InstrStop> {
    // Disabled runners must not hash code merely to discover that lookup is unavailable.
    // Compiled code treats the remaining gas as signed, so it cannot hold more than `i64::MAX`.
    if !backend.enabled() || interpreter.gas().remaining() > i64::MAX as u64 {
        return None;
    }
    let code_hash = interpreter.original_bytecode_hash();
    let code = interpreter.original_bytecode();
    let decision = backend.lookup(LookupRequest {
        key: RuntimeCacheKey { code_hash, spec_id: host.execution_config().base_spec_id() },
        code,
    });

    let program = match decision {
        LookupDecision::Compiled(program) => program,
        LookupDecision::Unavailable(_) => return None,
    };

    interpreter.prepare_run(host);
    Some(unsafe { program.func.call_with_interpreter(interpreter) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use evm2::{
        BaseEvmTypes, Evm, Precompiles, SpecId,
        bytecode::Bytecode,
        env::{BlockEnvExt, TxEnvExt},
        ethereum::ethereum_tx_registry,
        evm::EmptyDB,
        interpreter::{MessageExt, op},
    };

    #[cfg(feature = "llvm")]
    use crate::runtime::RuntimeConfig;
    #[cfg(feature = "llvm")]
    use alloy_primitives::{Address, hex};
    #[cfg(feature = "llvm")]
    use evm2::{
        EvmFeatures, ExecutionConfig, Version,
        env::BlockEnv,
        evm::{AccountInfo, InMemoryDB},
        interpreter::{Host, Word},
        version::GasId,
    };

    #[cfg(feature = "llvm")]
    const BYTECODE_RET42: &[u8] =
        &[op::PUSH1, 0x42, op::PUSH0, op::MSTORE, op::PUSH1, 0x20, op::PUSH0, op::RETURN];

    #[cfg(feature = "llvm")]
    fn blocking_backend() -> JitBackend {
        JitBackend::new(RuntimeConfig { enabled: true, blocking: true, ..RuntimeConfig::default() })
            .unwrap()
    }

    /// Runs `code` under `version` over `spec`, returning the stop reason, output, and gas spent.
    #[cfg(feature = "llvm")]
    fn run_with_version(
        spec: SpecId,
        version: Version,
        block: BlockEnvExt,
        code: &[u8],
        gas_limit: u64,
        with_jit: bool,
    ) -> (InstrStop, Bytes, u64) {
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            gas_params: Some(version.gas_params),
            ..RuntimeConfig::default()
        })
        .unwrap();
        let mut host = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(spec, version),
            spec,
            BlockEnv::<BaseEvmTypes>::from(block),
            ethereum_tx_registry(spec),
            InMemoryDB::default(),
            Precompiles::base(spec),
        );
        let message = MessageExt {
            gas_limit,
            code: Bytecode::new_legacy(Bytes::copy_from_slice(code)),
            ..MessageExt::default()
        };
        let tx_env = TxEnvExt::default();
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);
        let stop = if with_jit {
            run_interpreter(&backend, &mut interpreter, &mut host).unwrap()
        } else {
            interpreter.run(&mut host).unwrap()
        };
        (stop, Bytes::copy_from_slice(interpreter.output()), interpreter.gas().spent())
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
        let tx_env = TxEnvExt::default();
        let message = MessageExt {
            gas_limit: 1_000_000,
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..Default::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        assert_eq!(run_interpreter(&JitBackend::disabled(), &mut interpreter, &mut host), None);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn gas_above_i64_max_falls_back_to_interpreter() {
        let tx_env = TxEnvExt::default();
        let message = MessageExt {
            gas_limit: i64::MAX as u64 + 1,
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..Default::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        assert_eq!(run_interpreter(&blocking_backend(), &mut interpreter, &mut host), None);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_call_executes_message() {
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
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        let tx_env = TxEnvExt::default();
        // The call output overwrites the word stored before it.
        let mut code = vec![op::PUSH1, 0x69, op::PUSH0, op::MSTORE];
        code.extend([op::PUSH1, 0x20, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0]);
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
        let message = MessageExt {
            gas_limit: 1_000_000,
            destination: caller,
            code: Bytecode::new_legacy(code.into()),
            ..MessageExt::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);

        assert_eq!(run_interpreter(&backend, &mut interpreter, &mut host), Some(InstrStop::Return));
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 0x42);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_create_executes_message() {
        let creator = Address::from([0x33; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&creator, AccountInfo::default());
        let backend = blocking_backend();
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        let tx_env = TxEnvExt::default();
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
        let message = MessageExt {
            gas_limit: 1_000_000,
            destination: creator,
            code: Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
            ..MessageExt::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);

        assert_eq!(run_interpreter(&backend, &mut interpreter, &mut host), Some(InstrStop::Return));
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 1);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_staticcall_zeros_child_callvalue() {
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
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        let tx_env = TxEnvExt::default();
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
        let message = MessageExt {
            gas_limit: 1_000_000,
            destination: caller,
            value: Word::from(30),
            code: Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
            ..MessageExt::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);

        assert_eq!(run_interpreter(&backend, &mut interpreter, &mut host), Some(InstrStop::Return));
        assert_eq!(interpreter.output().len(), 32);
        assert_eq!(interpreter.output()[31], 0);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_staticcall_child_callvalue_does_not_transfer() {
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
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        let tx_env = TxEnvExt::default();
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
        let message = MessageExt {
            gas_limit: 1_000_000,
            destination: caller,
            value: Word::from(30),
            code: Bytecode::new_legacy(Bytes::copy_from_slice(&code)),
            ..MessageExt::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);

        assert_eq!(run_interpreter(&backend, &mut interpreter, &mut host), Some(InstrStop::Stop));
        let beneficiary_balance = host.read_account_info(&beneficiary).unwrap().unwrap().balance;
        assert_eq!(beneficiary_balance, initial_beneficiary_balance);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_dynamic_call_to_staticcall_loop_matches_interpreter_gas() {
        let caller = Address::from([0x11; 20]);
        let outer = Address::with_last_byte(0xfd);
        let middle = Address::with_last_byte(0xed);
        let leaf = Address::with_last_byte(0xdd);
        let outer_code = dynamic_call_outer_code();
        let mut input = [0u8; 32];
        input[12..].copy_from_slice(middle.as_slice());
        let message = MessageExt {
            gas_limit: 0xcd79195900 - 21_368,
            destination: outer,
            caller,
            input: Bytes::copy_from_slice(&input).into(),
            value: Word::from(10),
            code: Bytecode::new_legacy(Bytes::copy_from_slice(&outer_code)),
            code_address: outer,
            ..MessageExt::default()
        };
        let tx_env = TxEnvExt { gas_price: Word::from(10), ..TxEnvExt::default() };

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
                BlockEnvExt::default(),
                ethereum_tx_registry(SpecId::CANCUN),
                database,
                Precompiles::base(SpecId::CANCUN),
            );
            host.state_mut().prewarm(&caller);
            host.state_mut().prewarm(&outer);
            if with_jit {
                host.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
            }
            let mut interpreter = Interpreter::<BaseEvmTypes>::new(&tx_env, &message);
            let stop = if with_jit {
                run_interpreter(&backend, &mut interpreter, &mut host).unwrap()
            } else {
                interpreter.run(&mut host).unwrap()
            };
            (stop, interpreter.gas().spent(), interpreter.gas().refunded())
        };

        let interpreter = run(false);
        let jit = run(true);
        assert_eq!(jit, interpreter);
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_precompile_dispatch_uses_original_callee() {
        let sha256 = Address::with_last_byte(2);
        let delegate = Address::with_last_byte(0x33);
        let parent = Address::with_last_byte(0x44);
        let caller = Address::with_last_byte(0xaa);
        let sha256_output =
            hex!("9f64a747e1b97f131fabb6b447296c9b6f0201e79fb3c5356e6c77e89b6a806a");
        for (callee, delegated, expected_output) in [
            (sha256, delegate, sha256_output.as_slice()),
            (sha256, sha256, sha256_output.as_slice()),
            (Address::with_last_byte(0x55), sha256, &[]),
        ] {
            for opcode in [op::CALL, op::CALLCODE, op::DELEGATECALL, op::STATICCALL] {
                // Copy input, call with no output buffer, then return all return data.
                let mut code = vec![op::CALLDATASIZE, op::PUSH0, op::PUSH0, op::CALLDATACOPY];
                code.extend([op::PUSH0, op::PUSH0, op::PUSH1, 4, op::PUSH0]);
                if matches!(opcode, op::CALL | op::CALLCODE) {
                    code.push(op::PUSH0);
                }
                push20(&mut code, callee);
                code.extend([
                    op::GAS,
                    opcode,
                    op::POP,
                    op::RETURNDATASIZE,
                    op::PUSH0,
                    op::PUSH0,
                    op::RETURNDATACOPY,
                    op::RETURNDATASIZE,
                    op::PUSH0,
                    op::RETURN,
                ]);
                let message = MessageExt {
                    gas_limit: 100_000,
                    destination: parent,
                    call_target: parent,
                    code_address: parent,
                    caller,
                    input: Bytes::from_static(&hex!("01020304")).into(),
                    code: Bytecode::new_legacy(code.into()),
                    ..Default::default()
                };
                let run = |with_jit| {
                    let mut database = InMemoryDB::default();
                    database.insert_account_info(
                        &callee,
                        AccountInfo::default().with_code(Bytecode::new_eip7702(delegated)),
                    );
                    database.insert_account_info(
                        &delegate,
                        AccountInfo::default()
                            .with_code(Bytecode::new_legacy(Bytes::from_static(BYTECODE_RET42))),
                    );
                    let mut host = Evm::<BaseEvmTypes>::new(
                        SpecId::PRAGUE,
                        BlockEnvExt::default(),
                        ethereum_tx_registry(SpecId::PRAGUE),
                        database,
                        Precompiles::base(SpecId::PRAGUE),
                    );
                    host.warm_precompiles();
                    host.state_mut().prewarm(&caller);
                    host.state_mut().prewarm(&parent);
                    if with_jit {
                        host.set_interpreter_runner(JitInterpreterRunner::new(blocking_backend()));
                    }
                    Host::execute_message(&mut host, &TxEnvExt::default(), &mut message.clone())
                        .unwrap()
                };
                let interpreted = run(false);
                let compiled = run(true);
                assert_eq!(compiled.stop, InstrStop::Return);
                assert_eq!(compiled.output.as_ref(), expected_output);
                assert_eq!(compiled, interpreted);
            }
        }
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_difficulty_uses_active_features() {
        let block = BlockEnvExt {
            difficulty: Word::from(2),
            prevrandao: Word::from(7),
            ..BlockEnvExt::default()
        };
        for (spec, enabled, expected) in [(SpecId::CANCUN, false, 2), (SpecId::LONDON, true, 7)] {
            let mut version = Version::new(spec);
            if enabled {
                version.features.insert(EvmFeatures::EIP4399);
            } else {
                version.features.remove(EvmFeatures::EIP4399);
            }
            // PUSH0 is not active before Shanghai.
            let zero: &[u8] =
                if spec.enables(SpecId::SHANGHAI) { &[op::PUSH0] } else { &[op::PUSH1, 0] };
            let code =
                [&[op::DIFFICULTY][..], zero, &[op::MSTORE, op::PUSH1, 0x20], zero, &[op::RETURN]]
                    .concat();
            let run = |with_jit| run_with_version(spec, version, block, &code, 100_000, with_jit);
            let interpreted = run(false);
            assert_eq!(interpreted.0, InstrStop::Return);
            assert_eq!(Word::from_be_slice(&interpreted.1), Word::from(expected));
            assert_eq!(run(true), interpreted);
        }
    }

    #[test]
    #[cfg(feature = "llvm")]
    fn compiled_sstore_stipend_uses_active_features() {
        let mut version = Version::new(SpecId::PETERSBURG);
        version.features.insert(EvmFeatures::EIP2200);
        version.gas_params.set(GasId::SstoreStatic, 800);
        let mut code = vec![op::PUSH1, 0, op::PUSH1, 0, op::SSTORE];
        code.extend([op::PUSH1, 0, op::POP].repeat(200));
        code.push(op::STOP);
        let run = |with_jit| {
            run_with_version(
                SpecId::PETERSBURG,
                version,
                BlockEnvExt::default(),
                &code,
                3000,
                with_jit,
            )
        };
        let interpreted = run(false);
        assert_eq!(interpreted.0, InstrStop::Stop);
        assert_eq!(interpreted.2, 1806);
        assert_eq!(run(true), interpreted);
    }
}
