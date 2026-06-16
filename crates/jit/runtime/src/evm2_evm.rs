//! evm2 JIT interpreter dispatch helpers.

use crate::runtime::{InterpretReason, JitBackend, LookupDecision, LookupRequest, RuntimeCacheKey};
use alloy_primitives::{Bytes, keccak256};
use evm2::{
    EvmTypes, ExecutionConfig,
    interpreter::{InstrStop, Interpreter},
};

/// Result of trying to execute an evm2 interpreter frame through JIT code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitRunResult {
    /// No compiled program was available, so the caller should run the evm2 interpreter.
    Interpret(InterpretReason),
    /// Compiled execution finished the frame.
    Finished(InstrStop),
    /// Compiled execution suspended for a nested CALL/CREATE frame.
    Suspended,
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
    match unsafe { program.evm2_func::<T>().call_with_interpreter(interpreter, host) } {
        Some(stop) => JitRunResult::Finished(stop),
        None => JitRunResult::Suspended,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm2::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmConfigSelector, Precompiles, SpecId,
        bytecode::Bytecode,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::EmptyDB,
        interpreter::{Message, op},
    };

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
}
