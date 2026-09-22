//! EVM execution inspection hooks.

use crate::{
    EvmTypesHost,
    evm::NonStaticAny,
    interpreter::{Interpreter, Message, MessageResult},
};
use alloc::boxed::Box;
use alloy_primitives::{Address, Log, U256};
use auto_impl::auto_impl;

/// EVM execution inspector.
#[auto_impl(&mut, Box)]
pub trait Inspector<T: EvmTypesHost>: NonStaticAny {
    /// Called after a frame interpreter has been initialized.
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called before each instruction executes.
    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called after each instruction executes.
    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called when a log is emitted.
    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        let _ = log;
        let _ = host;
    }

    /// Called before a call message executes.
    ///
    /// The interpreter is the currently running frame whose instruction produced the message; for
    /// the top-level message it is a frame initialized with the message itself.
    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let _ = interp;
        let _ = message;
        None
    }

    /// Called after a call message executes.
    #[inline]
    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        let _ = interp;
        let _ = message;
        let _ = result;
    }

    /// Called before a create message executes.
    ///
    /// The interpreter is the currently running frame whose instruction produced the message; for
    /// the top-level message it is a frame initialized with the message itself.
    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let _ = interp;
        let _ = message;
        None
    }

    /// Called after a create message executes.
    #[inline]
    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        let _ = interp;
        let _ = message;
        let _ = result;
    }

    /// Called after a contract self-destructs.
    #[inline]
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        let _ = contract;
        let _ = target;
        let _ = value;
        let _ = host;
    }
}

#[inline]
pub(crate) fn boxed_inspector<'a, T: EvmTypesHost>(
    inspector: impl Inspector<T> + 'a,
) -> Box<dyn Inspector<T> + 'a> {
    Box::new(inspector)
}

/// Inspector that does nothing.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Default)]
pub struct NoopInspector(());

impl<T: EvmTypesHost> Inspector<T> for NoopInspector {}

impl<'a, T: EvmTypesHost> core::ops::Deref for dyn Inspector<T> + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a, T: EvmTypesHost> core::ops::DerefMut for dyn Inspector<T> + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::Inspector;
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmTypesHost, ExecutionConfig, Precompiles,
        SpecId,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::{BlockEnvExt, TxEnvExt},
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, EmptyDB, InMemoryDB, SYSTEM_ADDRESS, State},
        interpreter::{
            GasTracker, Host, InstrStop, Interpreter, Message, MessageExt, MessageResult,
            MessageResultExt, Word, op,
        },
        registry::TxRegistry,
        test_utils::{TestHost, TestTypes, legacy_bytecode, push, push_all},
        utils::address_to_word,
    };
    use alloc::{boxed::Box, vec, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
    use core::assert_matches;

    #[derive(Default)]
    struct SelfdestructInspector {
        selfdestruct: Option<(Address, Address, Word)>,
    }

    impl<T: EvmTypesHost> Inspector<T> for SelfdestructInspector {
        fn selfdestruct(
            &mut self,
            contract: &Address,
            target: &Address,
            value: &Word,
            _host: &mut T::Host<'_>,
        ) {
            self.selfdestruct = Some((*contract, *target, *value));
        }
    }

    #[derive(Default)]
    struct HookInspector {
        call_depths: Vec<u16>,
        call_end_stops: Vec<InstrStop>,
        create_depths: Vec<u16>,
        create_destinations: Vec<Address>,
        create_end_stops: Vec<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for HookInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.call_depths.push(message.depth);
            None
        }

        fn call_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.call_end_stops.push(result.stop);
        }

        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.create_depths.push(message.depth);
            self.create_destinations.push(message.destination);
            None
        }

        fn create_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.create_end_stops.push(result.stop);
        }
    }

    struct OverrideCallInspector {
        result: MessageResult<BaseEvmTypes>,
        min_depth: u16,
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for OverrideCallInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            if message.depth < self.min_depth {
                return None;
            }
            self.call_depth = Some(message.depth);
            let mut result = self.result.clone();
            result.gas.set_remaining(message.gas_limit);
            Some(result)
        }

        fn call_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            if message.depth >= self.min_depth {
                self.call_end_stop = Some(result.stop);
            }
        }
    }

    struct OverrideCreateInspector {
        created: Address,
        create_depth: Option<u16>,
        create_end_stop: Option<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for OverrideCreateInspector {
        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.create_depth = Some(message.depth);
            Some(MessageResultExt {
                stop: InstrStop::Return,
                gas: GasTracker::new(message.gas_limit),
                created_address: Some(self.created),
                ..Default::default()
            })
        }

        fn create_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.create_end_stop = Some(result.stop);
        }
    }

    #[derive(Default)]
    struct LogInspector {
        logs: Vec<Log>,
    }

    impl<T: EvmTypesHost> Inspector<T> for LogInspector {
        fn log(&mut self, log: &Log, _host: &mut T::Host<'_>) {
            self.logs.push(log.clone());
        }
    }

    #[derive(Default)]
    struct E2eState {
        initialized: usize,
        steps: usize,
        step_ends: usize,
        logs: Vec<Log>,
        calls: usize,
        creates: usize,
    }

    #[derive(Default)]
    struct SharedE2eInspector {
        state: E2eState,
    }

    impl Inspector<BaseEvmTypes> for SharedE2eInspector {
        fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.state.initialized += 1;
        }

        fn step(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.state.steps += 1;
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.state.step_ends += 1;
        }

        fn log(&mut self, log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
            self.state.logs.push(log.clone());
        }

        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.calls += 1;
            None
        }

        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.creates += 1;
            None
        }
    }

    fn run_evm_with_inspector<I: Inspector<BaseEvmTypes> + 'static>(
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<'static, BaseEvmTypes>) {
        run_evm_with_inspector_db(InMemoryDB::default(), code, message, gas_limit, inspector)
    }

    fn run_evm_with_inspector_db<I: Inspector<BaseEvmTypes> + 'static>(
        db: InMemoryDB,
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<'static, BaseEvmTypes>) {
        run_evm_with_inspector_db_spec(SpecId::OSAKA, db, code, message, gas_limit, inspector)
    }

    fn run_evm_with_inspector_db_spec<I: Inspector<BaseEvmTypes> + 'static>(
        spec_id: SpecId,
        db: InMemoryDB,
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<'static, BaseEvmTypes>) {
        let mut evm = Evm::<BaseEvmTypes>::new(
            spec_id,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(spec_id),
        );
        evm.set_inspector(inspector);
        let tx_env = TxEnvExt::default();
        let bytecode = legacy_bytecode(code);
        let mut message = MessageExt { gas_limit, code: bytecode, ..message.clone() };
        let result = Host::execute_message(&mut evm, &tx_env, &mut message);
        let inspector = evm.clear_inspector_as::<I>().unwrap();
        (result, inspector, evm)
    }

    /// Appends code that returns the word at the top of the stack as the frame output.
    fn return_top_word(code: &mut Vec<u8>) {
        code.extend([op::PUSH0, op::MSTORE, op::PUSH1, 32, op::PUSH0, op::RETURN]);
    }

    fn call_code(target: Address) -> Vec<u8> {
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                address_to_word(&target),
                Word::from(1000),
            ],
        );
        code
    }

    fn create_code() -> Vec<u8> {
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code
    }

    #[test]
    fn inspect_run_steps() {
        #[derive(Default)]
        struct StepInspector {
            steps: usize,
            step_ends: usize,
        }

        impl<T: EvmTypesHost> Inspector<T> for StepInspector {
            fn step(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::STOP]),
            &MessageExt::default(),
            10_000,
            StepInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    #[test]
    fn inspector_gas_edits_survive_dispatch() {
        #[derive(Default)]
        struct GasInspector {
            steps: usize,
            ends: usize,
        }

        impl Inspector<BaseEvmTypes> for GasInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let mut gas = interp.gas();
                gas.set_remaining(100);
                gas.set_refunded(7);
                interp.set_gas(gas);
            }

            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let expected = [100, 128, 156, 184, 212, 236][self.steps];
                assert_eq!(interp.gas().remaining(), expected);
                assert_eq!(interp.gas().refunded(), 7);
                interp.gas_mut().set_remaining(expected + 10);
                self.steps += 1;
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let expected = [108, 136, 164, 192, 216, 246][self.ends];
                assert_eq!(interp.gas().remaining(), expected);
                if self.ends == 1 {
                    assert_eq!(interp.stack().last(), Some(&Word::from(136)));
                }
                interp.gas_mut().set_remaining(expected + 20);
                self.ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH0, op::GAS, op::POP, op::PUSH0, op::MSTORE, op::STOP]),
            &MessageExt::default(),
            1000,
            GasInspector::default(),
        );
        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(result.gas.remaining(), 266);
        assert_eq!(result.gas.refunded(), 7);
        assert_eq!((inspector.steps, inspector.ends), (6, 6));
    }

    #[test]
    fn inspector_message_hooks_preserve_parent_gas_edits() {
        #[derive(Default)]
        struct GasInspector {
            opcode: u8,
            before: u64,
            calls: usize,
            ends: usize,
        }

        impl Inspector<BaseEvmTypes> for GasInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                self.opcode = interp.opcode();
                self.before = interp.gas().remaining();
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if matches!(self.opcode, op::CALL | op::CREATE) {
                    assert_eq!(interp.gas().remaining(), 4100);
                } else if self.opcode == op::GAS {
                    assert_eq!(interp.stack().last(), Some(&Word::from(4098)));
                }
            }

            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 0 {
                    return None;
                }
                let expected = if self.opcode == op::CALL {
                    self.before - 2600 - message.gas_limit
                } else {
                    self.before - 32000 - message.gas_limit
                };
                assert_eq!(interp.gas().remaining(), expected);
                interp.gas_mut().set_remaining(5000);
                self.calls += 1;
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(100),
                    created_address: Some(Address::from([0x77; 20])),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                _result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.depth > 0 {
                    assert_eq!(interp.gas().remaining(), 5000);
                    interp.gas_mut().set_remaining(4000);
                    self.ends += 1;
                }
            }

            fn create(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                self.call(interp, message)
            }

            fn create_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                self.call_end(interp, message, result);
            }
        }

        for (mut code, opcode) in
            [(call_code(Address::from([0x66; 20])), op::CALL), (create_code(), op::CREATE)]
        {
            code.extend([opcode, op::GAS, op::STOP]);
            let (result, inspector, _) = run_evm_with_inspector(
                code,
                &MessageExt::default(),
                100_000,
                GasInspector::default(),
            );
            assert_eq!(result.stop, InstrStop::Stop);
            assert_eq!(result.gas.remaining(), 4098);
            assert_eq!((inspector.calls, inspector.ends), (1, 1));
        }
    }

    #[test]
    fn inspector_gas_is_valid_after_out_of_gas() {
        struct GasInspector {
            recover: bool,
            failures: usize,
        }

        impl Inspector<BaseEvmTypes> for GasInspector {
            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if matches!(interp.result(), Err(InstrStop::OutOfGas | InstrStop::MemoryOOG)) {
                    assert_eq!(interp.gas().remaining(), 0);
                    self.failures += 1;
                    if self.recover {
                        interp.gas_mut().set_remaining(17);
                        interp.set_stop(InstrStop::Stop);
                    }
                }
            }
        }

        for (code, gas_limit, stop) in [
            (vec![op::PUSH0], 1, InstrStop::OutOfGas),
            (vec![op::PUSH0, op::PUSH0, op::MSTORE], 9, InstrStop::MemoryOOG),
            (vec![op::PUSH0, op::SLOAD], 2, InstrStop::OutOfGas),
        ] {
            for recover in [false, true] {
                let (result, inspector, _) = run_evm_with_inspector(
                    code.clone(),
                    &MessageExt::default(),
                    gas_limit,
                    GasInspector { recover, failures: 0 },
                );
                assert_eq!(inspector.failures, 1, "code={code:?}, result={result:?}");
                assert_eq!(result.stop, if recover { InstrStop::Stop } else { stop });
                assert_eq!(result.gas.remaining(), if recover { 17 } else { 0 });
            }
        }
    }

    #[test]
    fn step_can_stop_before_current_opcode_executes() {
        #[derive(Default)]
        struct StopOnStepInspector {
            opcode: u8,
            steps: usize,
            step_ends: usize,
            stack: Vec<Word>,
        }

        impl<T: EvmTypesHost> Inspector<T> for StopOnStepInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
                if interp.opcode() == self.opcode {
                    self.stack = interp.stack().to_vec();
                    interp.gas_mut().set_remaining(123);
                    interp.set_stop(InstrStop::Revert);
                }
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &MessageExt::default(),
            10_000,
            StopOnStepInspector { opcode: op::ADD, ..Default::default() },
        );

        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(inspector.stack, [Word::from(1), Word::from(2)]);
        assert_eq!(inspector.steps, 3);
        assert_eq!(inspector.step_ends, 2);
        assert_eq!(result.gas.remaining(), 123);
    }

    #[test]
    fn step_end_can_stop_before_next_opcode_executes() {
        #[derive(Default)]
        struct StopOnStepEndInspector {
            opcode: u8,
            last_opcode: Option<u8>,
            steps: usize,
            step_ends: usize,
            stack: Vec<Word>,
        }

        impl<T: EvmTypesHost> Inspector<T> for StopOnStepEndInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
                self.last_opcode = Some(interp.opcode());
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                self.step_ends += 1;
                if self.last_opcode == Some(self.opcode) {
                    self.stack = interp.stack().to_vec();
                    interp.gas_mut().set_remaining(123);
                    interp.set_stop(InstrStop::Revert);
                }
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &MessageExt::default(),
            10_000,
            StopOnStepEndInspector { opcode: op::PUSH1, ..Default::default() },
        );

        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(inspector.stack, [Word::from(1)]);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
        assert_eq!(result.gas.remaining(), 123);
    }

    #[test]
    fn call_too_deep_is_inspected_without_executing() {
        let target = Address::from([0x22; 20]);
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.call_depths, [CALL_DEPTH_LIMIT, CALL_DEPTH_LIMIT + 1]);
        assert_eq!(inspector.call_end_stops, [InstrStop::CallTooDeep, InstrStop::Stop]);
    }

    #[test]
    fn call_inspector_override_skips_execution_and_still_calls_end() {
        let target = Address::from([0x22; 20]);
        let inspector = OverrideCallInspector {
            result: MessageResultExt {
                stop: InstrStop::Return,
                output: Bytes::from_static(&[0xaa, 0xbb, 0xcc]),
                ..Default::default()
            },
            min_depth: 1,
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL, op::POP, op::RETURNDATASIZE]);
        return_top_word(&mut code);

        let (result, inspector, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, inspector);

        assert_matches!(result.stop, InstrStop::Return);
        // The override output is observed by the parent frame's RETURNDATASIZE.
        assert_eq!(Word::from_be_slice(&result.output), Word::from(3));
        assert_eq!(inspector.call_depth, Some(1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn call_inspector_override_wins_at_max_depth() {
        let target = Address::from([0x22; 20]);
        let inspector = OverrideCallInspector {
            result: MessageResultExt { stop: InstrStop::Return, ..Default::default() },
            min_depth: CALL_DEPTH_LIMIT + 1,
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL]);
        return_top_word(&mut code);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            inspector,
        );

        assert_matches!(result.stop, InstrStop::Return);
        // The override wins over the call depth check: the call succeeds.
        assert_eq!(Word::from_be_slice(&result.output), Word::from(1));
        assert_eq!(inspector.call_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn call_inspector_can_mutate_message_before_execution() {
        struct MutateCallInspector {
            destination: Address,
        }

        impl Inspector<BaseEvmTypes> for MutateCallInspector {
            fn call(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth > 0 {
                    message.destination = self.destination;
                    message.code_address = self.destination;
                }
                None
            }
        }

        let target = Address::from([0x22; 20]);
        let replacement = Address::from([0x33; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &Address::ZERO,
            AccountInfo::default().with_balance(U256::from(100)),
        );
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::from(7),
                address_to_word(&target),
                Word::from(50_000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(MutateCallInspector { destination: replacement });
        let tx_env = TxEnvExt::default();
        let bytecode = legacy_bytecode(code);
        let mut message = MessageExt { gas_limit: 100_000, code: bytecode, ..Default::default() };
        let result = Host::execute_message(&mut evm, &tx_env, &mut message);

        assert_matches!(result.stop, InstrStop::Stop);
        // The redirected call transferred the value to the replacement, not the target.
        let mut balance = |address| {
            evm.state_mut()
                .account_info_untracked(&address)
                .unwrap()
                .map_or(U256::ZERO, |info| info.balance)
        };
        assert_eq!(balance(replacement), U256::from(7));
        assert_eq!(balance(target), U256::ZERO);
    }

    #[test]
    fn call_end_can_mutate_result_before_caller_observes_it() {
        struct CallEndInspector;

        impl Inspector<BaseEvmTypes> for CallEndInspector {
            fn call(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 0 {
                    return None;
                }
                Some(MessageResultExt {
                    stop: InstrStop::Revert,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.depth > 0 {
                    result.stop = InstrStop::Return;
                    result.output = Bytes::from_static(&[0xaa, 0xbb]);
                }
            }
        }

        let target = Address::from([0x22; 20]);
        let mut code = call_code(target);
        code.extend([op::CALL, op::POP, op::RETURNDATASIZE]);
        return_top_word(&mut code);

        let (result, _, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, CallEndInspector);

        assert_matches!(result.stop, InstrStop::Return);
        // `call_end` upgraded the override from a revert to a 2-byte return.
        assert_eq!(Word::from_be_slice(&result.output), Word::from(2));
    }

    #[test]
    fn create_too_deep_is_inspected_without_executing() {
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.create_depths, [CALL_DEPTH_LIMIT + 1]);
        assert_eq!(inspector.create_end_stops, [InstrStop::CallTooDeep]);
    }

    #[test]
    fn create_inspector_override_skips_execution_and_still_calls_end() {
        let created = Address::from([0x77; 20]);
        let inspector =
            OverrideCreateInspector { created, create_depth: None, create_end_stop: None };
        let mut code = create_code();
        code.extend([op::CREATE]);
        return_top_word(&mut code);

        let (result, inspector, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, inspector);

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), address_to_word(&created));
        assert_eq!(inspector.create_depth, Some(1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn create_inspector_sees_derived_destination() {
        let contract = Address::from([0x11; 20]);
        let expected = contract.create(0);
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { destination: contract, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.create_destinations, [expected]);
    }

    #[test]
    fn amsterdam_create_preaccess_failure_does_not_fire_create_hook() {
        let contract = Address::from([0x11; 20]);
        for is_create2 in [false, true] {
            for (balance, nonce, value, depth) in [
                (Word::from(1), 0, Word::from(2), 0),
                (Word::MAX, u64::MAX, Word::ZERO, 0),
                (Word::MAX, 0, Word::ZERO, CALL_DEPTH_LIMIT),
            ] {
                let mut code = Vec::new();
                if is_create2 {
                    push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO, value]);
                } else {
                    push_all(&mut code, [Word::ZERO, Word::ZERO, value]);
                }
                code.push(if is_create2 { op::CREATE2 } else { op::CREATE });
                return_top_word(&mut code);

                let mut db = InMemoryDB::default();
                db.insert_account_info(
                    &contract,
                    AccountInfo { balance, nonce, ..Default::default() },
                );
                let (result, inspector, _) = run_evm_with_inspector_db_spec(
                    SpecId::AMSTERDAM,
                    db,
                    code,
                    &MessageExt { destination: contract, depth, ..Default::default() },
                    100_000,
                    HookInspector::default(),
                );

                assert_matches!(result.stop, InstrStop::Return);
                assert_eq!(Word::from_be_slice(&result.output), Word::ZERO);
                assert!(inspector.create_depths.is_empty());
                assert!(inspector.create_end_stops.is_empty());
            }
        }
    }

    #[test]
    fn create_inspector_override_wins_at_max_depth() {
        let created = Address::from([0x77; 20]);
        let inspector =
            OverrideCreateInspector { created, create_depth: None, create_end_stop: None };
        let mut code = create_code();
        code.extend([op::CREATE]);
        return_top_word(&mut code);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            inspector,
        );

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), address_to_word(&created));
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn create_end_can_mutate_result_before_caller_observes_it() {
        struct CreateEndInspector {
            created: Address,
        }

        impl Inspector<BaseEvmTypes> for CreateEndInspector {
            fn create(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                Some(MessageResultExt {
                    stop: InstrStop::Revert,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn create_end(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                result.stop = InstrStop::Return;
                result.created_address = Some(self.created);
            }
        }

        let created = Address::from([0x88; 20]);
        let mut code = create_code();
        code.extend([op::CREATE]);
        return_top_word(&mut code);

        let (result, _, _) = run_evm_with_inspector(
            code,
            &MessageExt::default(),
            50_000,
            CreateEndInspector { created },
        );

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), address_to_word(&created));
    }

    #[test]
    fn log_opcode_is_inspected_and_emitted_to_host() {
        let contract = Address::from([0x11; 20]);
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (result, inspector, evm) = run_evm_with_inspector(
            code,
            &MessageExt { destination: contract, ..Default::default() },
            10_000,
            LogInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.logs.len(), 1);
        assert_eq!(inspector.logs[0].address, contract);
        assert_eq!(evm.logs(), inspector.logs);
    }

    #[test]
    fn log_opcode_oog_is_not_inspected_or_emitted_to_host() {
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (result, inspector, evm) =
            run_evm_with_inspector(code, &MessageExt::default(), 6, LogInspector::default());

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert!(inspector.logs.is_empty());
        assert!(evm.logs().is_empty());
    }

    #[test]
    fn step_end_runs_for_failing_opcode_with_result_set() {
        #[derive(Default)]
        struct FailingStepInspector {
            steps: usize,
            step_ends: usize,
        }

        impl<T: EvmTypesHost> Inspector<T> for FailingStepInspector {
            fn step(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                let _ = interp;
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::INVALID]),
            &MessageExt::default(),
            10_000,
            FailingStepInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::InvalidFEOpcode);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    #[test]
    fn selfdestruct_is_inspected_from_opcode() {
        let contract = Address::from([0x11; 20]);
        let target = Address::from([0x99; 20]);
        let value = Word::from(0xbeef);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&contract, AccountInfo::default().with_balance(value));
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let (result, inspector, _) = run_evm_with_inspector_db(
            db,
            code,
            &MessageExt { destination: contract, ..Default::default() },
            50_000,
            SelfdestructInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::SelfDestruct);
        assert_eq!(inspector.selfdestruct, Some((contract, target, value)));
    }

    #[test]
    fn selfdestruct_dynamic_gas_oog_is_not_inspected() {
        let contract = Address::from([0x11; 20]);
        let target = Address::from([0x99; 20]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&contract, AccountInfo::default());
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let (result, inspector, _) = run_evm_with_inspector_db(
            db,
            code,
            &MessageExt { destination: contract, ..Default::default() },
            7_000,
            SelfdestructInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert_eq!(inspector.selfdestruct, None);
    }

    #[test]
    fn selfdestruct_host_error_is_not_inspected() {
        // Host failures are injected through the mock host; this intentionally uses [`TestHost`].
        let target = Address::from([0x99; 20]);
        let mut host = TestHost {
            selfdestruct_error: Some(InstrStop::FatalExternalError),
            ..Default::default()
        };
        let mut inspector = SelfdestructInspector::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let tx_env = TxEnvExt::default();
        let message = Message::<TestTypes> {
            gas_limit: 10_000,
            code: legacy_bytecode(code),
            ..Default::default()
        };
        let mut interp = Interpreter::<TestTypes>::new(&tx_env, &message);
        let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let stop = interp.run_inspect(&config, &mut host, &mut inspector);

        assert_eq!(stop, InstrStop::FatalExternalError);
        assert_eq!(inspector.selfdestruct, None);
    }

    #[test]
    fn evm_transaction_inspects_interpreter_steps_and_logs() {
        let caller = Address::from([0xaa; 20]);
        let contract = Address::from([0xbb; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::LOG0,
            op::STOP,
        ]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(contract),
                gas_limit: 100_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").discard();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.status);
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 4);
        assert_eq!(state.step_ends, 4);
        assert_eq!(state.logs.len(), 1);
        assert_eq!(state.logs[0].address, contract);
        assert_eq!(state.calls, 1);
        assert_eq!(state.creates, 0);
    }

    #[test]
    fn evm_transaction_inspects_eip7708_transfer_log() {
        let caller = Address::from([0xaa; 20]);
        let target = Address::from([0xbb; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                value: U256::from(7),
                gas_limit: 300_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").detach();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.result.status);
        assert_eq!(result.result.logs.len(), 1);
        assert_eq!(state.logs, result.result.logs);
        assert_eq!(state.logs[0].address, SYSTEM_ADDRESS);
    }

    #[test]
    fn evm_create_transaction_initializes_interpreter_with_create_hook() {
        let caller = Address::from([0xaa; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Create,
                input: Bytes::from_static(&[op::STOP]),
                gas_limit: 100_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").discard();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.status);
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 1);
        assert_eq!(state.step_ends, 1);
        assert_eq!(state.calls, 0);
        assert_eq!(state.creates, 1);
    }

    #[test]
    fn cooling_loaded_slot_respects_access_list() {
        struct CoolSlotInspector {
            prewarmed: bool,
            before: Option<u64>,
            costs: Vec<u64>,
        }
        impl Inspector<BaseEvmTypes> for CoolSlotInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let address = interp.message().destination;
                let state = interp.host().state_mut();
                state.storage_slot(&address, Word::ZERO, false).unwrap().set(Word::from(7));
                if self.prewarmed {
                    state.prewarm_storage_slot(&address, Word::ZERO);
                }
            }
            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if interp.opcode() == op::SLOAD {
                    if self.costs.len() == 1 {
                        let address = interp.message().destination;
                        interp.host().state_mut().set_storage_warm(&address, Word::ZERO, false);
                    }
                    self.before = Some(interp.gas().remaining());
                }
            }
            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if let Some(before) = self.before.take() {
                    self.costs.push(before - interp.gas().remaining());
                }
            }
        }
        for prewarmed in [false, true] {
            let mut code = vec![
                op::PUSH0,
                op::SLOAD,
                op::POP,
                op::PUSH0,
                op::SLOAD,
                op::POP,
                op::PUSH0,
                op::SLOAD,
            ];
            return_top_word(&mut code);
            let (result, inspector, _) = run_evm_with_inspector(
                code,
                &MessageExt::default(),
                100_000,
                CoolSlotInspector { prewarmed, before: None, costs: Vec::new() },
            );
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(Word::from_be_slice(&result.output), Word::from(7));
            assert_eq!(
                inspector.costs,
                [if prewarmed { 100 } else { 2100 }, if prewarmed { 100 } else { 2100 }, 100]
            );
        }
    }

    /// Reproduces Foundry restoring a snapshot from a still-running child call.
    #[test]
    fn restored_snapshot_allows_child_and_parent_revert() {
        #[derive(Default)]
        struct SnapshotInspector {
            snapshot: Option<State<'static>>,
            restored: bool,
            child_reverted: bool,
        }

        impl Inspector<BaseEvmTypes> for SnapshotInspector {
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                let marker = message.destination;
                if marker == Address::with_last_byte(0x44) {
                    self.snapshot = Some(interp.host().state().clone_with(EmptyDB::default()));
                } else if marker == Address::with_last_byte(0x55) {
                    *interp.host().state_mut() =
                        self.snapshot.as_ref().unwrap().clone_with(EmptyDB::default());
                    self.restored = true;
                } else {
                    return None;
                }
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.destination == Address::with_last_byte(0xbb) {
                    assert_eq!(result.stop, InstrStop::Revert);
                    assert_eq!(
                        interp
                            .host()
                            .state_mut()
                            .storage_slot_untracked(&Address::with_last_byte(0xaa), &Word::ZERO,)
                            .unwrap(),
                        Word::from(7),
                    );
                    self.child_reverted = true;
                }
            }
        }

        fn append_call(code: &mut Vec<u8>, target: Address) {
            push_all(
                code,
                [
                    Word::ZERO,
                    Word::ZERO,
                    Word::ZERO,
                    Word::ZERO,
                    Word::ZERO,
                    address_to_word(&target),
                    Word::from(200_000),
                ],
            );
            code.extend([op::CALL, op::POP]);
        }

        let parent = Address::with_last_byte(0xaa);
        let child = Address::with_last_byte(0xbb);
        let mut parent_code = vec![op::PUSH1, 7, op::PUSH0, op::SSTORE];
        append_call(&mut parent_code, Address::with_last_byte(0x44));
        parent_code.extend([op::PUSH1, 8, op::PUSH0, op::SSTORE]);
        append_call(&mut parent_code, child);
        parent_code.extend([op::PUSH0, op::PUSH0, op::REVERT]);
        let mut child_code = Vec::new();
        append_call(&mut child_code, Address::with_last_byte(0x55));
        child_code.extend([op::PUSH0, op::PUSH0, op::REVERT]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &child,
            AccountInfo::default().with_code(legacy_bytecode(child_code)),
        );
        let (result, inspector, mut evm) = run_evm_with_inspector_db(
            db,
            parent_code,
            &MessageExt { destination: parent, ..Default::default() },
            1_000_000,
            SnapshotInspector::default(),
        );
        assert!(inspector.restored);
        assert!(inspector.child_reverted);
        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(
            evm.state_mut().storage_slot_untracked(&parent, &Word::ZERO).unwrap(),
            Word::ZERO,
        );
    }

    #[test]
    fn snapshot_restore_preserves_logs_without_reinspection() {
        #[derive(Default)]
        struct SnapshotInspector {
            snapshot: Option<State<'static>>,
            logs: usize,
        }
        impl Inspector<BaseEvmTypes> for SnapshotInspector {
            fn log(&mut self, _log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
                self.logs += 1;
            }
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                match message.destination.as_slice()[19] {
                    0x44 => {
                        self.snapshot = Some(interp.host().state().clone_with(EmptyDB::default()))
                    }
                    0x55 => {
                        let state = interp.host().state_mut();
                        let logs = core::mem::take(state.logs_mut());
                        let db = core::mem::replace(
                            &mut state.overlay_db_mut().db,
                            Box::new(EmptyDB::default()),
                        );
                        *state = self.snapshot.as_ref().unwrap().clone_with(db);
                        *state.logs_mut() = logs;
                    }
                    _ => return None,
                }
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }
        }
        let mut code = Vec::new();
        for marker in [0x44, 0x55] {
            code.extend([
                op::PUSH1,
                marker,
                op::PUSH0,
                op::MSTORE8,
                op::PUSH1,
                1,
                op::PUSH0,
                op::LOG0,
            ]);
            code.extend(call_code(Address::with_last_byte(marker)));
            code.extend([op::CALL, op::POP]);
            if marker == 0x44 {
                code.extend([op::PUSH1, 9, op::PUSH0, op::SSTORE]);
            }
        }
        code.push(op::STOP);
        let (result, inspector, mut evm) = run_evm_with_inspector(
            code,
            &MessageExt::default(),
            100_000,
            SnapshotInspector::default(),
        );
        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(evm.logs().len(), 2);
        assert_eq!(evm.logs()[0].data.data.as_ref(), &[0x44]);
        assert_eq!(evm.logs()[1].data.data.as_ref(), &[0x55]);
        assert_eq!(inspector.logs, 2);
        assert_eq!(
            evm.state_mut().storage_slot_untracked(&Address::ZERO, &Word::ZERO).unwrap(),
            Word::ZERO
        );
    }

    #[test]
    fn isolated_transaction_resets_transient_state_and_merges_storage() {
        let target = Address::with_last_byte(0xbb);
        let caller = Address::with_last_byte(0x99);
        for reverts in [false, true] {
            let mut parent = State::new(EmptyDB::default());
            // Return the initial persistent and transient values, then write persistent storage.
            let code = vec![
                op::PUSH0,
                op::SLOAD,
                op::PUSH0,
                op::MSTORE,
                op::PUSH0,
                op::TLOAD,
                op::PUSH1,
                32,
                op::MSTORE,
                op::PUSH1,
                8,
                op::PUSH0,
                op::SSTORE,
                op::PUSH1,
                64,
                op::PUSH0,
                if reverts { op::REVERT } else { op::RETURN },
            ];
            parent.account(&target, false).unwrap().set_code_slow(legacy_bytecode(code));
            parent.account(&caller, false).unwrap().set_balance(Word::from(1_000_000_000));
            parent.storage_slot(&target, Word::ZERO, false).unwrap().set(Word::from(7));
            parent.storage_slot(&target, Word::ZERO, false).unwrap().warm();
            parent.tstore(&target, &Word::ZERO, &Word::from(9));
            let checkpoint = parent.checkpoint();
            let mut child = Evm::<BaseEvmTypes>::new(
                SpecId::CANCUN,
                BlockEnvExt::default(),
                ethereum_tx_registry(SpecId::CANCUN),
                EmptyDB::default(),
                Precompiles::base(SpecId::CANCUN),
            );
            child.state_mut().set_pending_state(parent.prepare_isolated_state());
            let tx = Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    to: TxKind::Call(target),
                    gas_limit: 100_000,
                    ..Default::default()
                }),
                caller,
            );
            let output = child.transact(&tx).unwrap().detach();
            assert_eq!(output.result.status, !reverts);
            assert_eq!(Word::from_be_slice(&output.result.output[..32]), Word::from(7));
            assert_eq!(Word::from_be_slice(&output.result.output[32..]), Word::ZERO);
            parent.merge_isolated_state(output.pending_state);
            assert_eq!(parent.checkpoint(), checkpoint);
            let slot = parent.storage_slot(&target, Word::ZERO, false).unwrap();
            assert_eq!(slot.original(), Word::ZERO);
            assert_eq!(slot.current(), Word::from(if reverts { 7 } else { 8 }));
            assert!(slot.is_warm());
            assert_eq!(parent.tload(&target, &Word::ZERO), Word::from(9));
        }
    }

    #[test]
    fn inspector_gas_changes_survive_dispatch_errors() {
        struct GasEdit;
        impl Inspector<TestTypes> for GasEdit {
            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, TestTypes>) {
                interp.gas_mut().set_remaining(1000);
            }
        }

        for (code, gas_limit, expected) in [
            (Vec::from([op::PUSH1, 0]), 2, InstrStop::OutOfGas),
            (Vec::from([op::INVALID]), 10_000, InstrStop::InvalidFEOpcode),
        ] {
            let tx_env = TxEnvExt::default();
            let message = Message::<TestTypes> {
                gas_limit,
                code: legacy_bytecode(code),
                ..Default::default()
            };
            let mut interp = Interpreter::<TestTypes>::new(&tx_env, &message);
            let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
            let stop = interp.run_inspect(&config, &mut TestHost::default(), &mut GasEdit);

            assert_eq!(stop, expected);
            assert_eq!(interp.gas().remaining(), 1000);
        }
    }
}
