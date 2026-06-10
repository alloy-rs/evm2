//! EVM execution inspection hooks.

use crate::{
    EvmTypes,
    interpreter::{Interpreter, Message, MessageResult},
};
use alloy_primitives::{Address, Log, U256};
use core::any::Any;

/// EVM execution inspector.
pub trait Inspector<T: EvmTypes>: Any {
    /// Called after a frame interpreter has been initialized.
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        let _ = interp;
    }

    /// Called before each instruction executes.
    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        let _ = interp;
    }

    /// Called after each instruction executes.
    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        let _ = interp;
    }

    /// Called when a log is emitted.
    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host) {
        let _ = log;
        let _ = host;
    }

    /// Called before a call message executes.
    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, T>,
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
        interp: &mut Interpreter<'_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        let _ = interp;
        let _ = message;
        let _ = result;
    }

    /// Called before a create message executes.
    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, T>,
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
        interp: &mut Interpreter<'_, T>,
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
        host: &mut T::Host,
    ) {
        let _ = contract;
        let _ = target;
        let _ = value;
        let _ = host;
    }
}

/// Inspector that does nothing.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Default)]
pub struct NoopInspector(());

impl<T: EvmTypes> Inspector<T> for NoopInspector {}

impl<T: EvmTypes> core::ops::Deref for dyn Inspector<T> + '_ {
    type Target = dyn Any;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<T: EvmTypes> core::ops::DerefMut for dyn Inspector<T> + '_ {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::Inspector;
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::{BlockEnv, TxEnv},
        ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, InMemoryDB, SYSTEM_ADDRESS, SelfDestructResult},
        interpreter::{
            GasTracker, InstrStop, Interpreter, Message, MessageResult, Word,
            instructions::tests::{TestHost, TestTypes, push},
            op,
        },
        utils::address_to_word,
    };
    use alloc::vec::Vec;
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
    use core::assert_matches;

    #[derive(Default)]
    struct StepInspector {
        steps: usize,
        step_ends: usize,
    }

    impl Inspector<TestTypes> for StepInspector {
        fn step(&mut self, _interp: &mut Interpreter<'_, TestTypes>) {
            self.steps += 1;
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, TestTypes>) {
            self.step_ends += 1;
        }
    }

    struct StopOnStepInspector {
        opcode: u8,
        steps: usize,
        step_ends: usize,
    }

    impl Inspector<TestTypes> for StopOnStepInspector {
        fn step(&mut self, interp: &mut Interpreter<'_, TestTypes>) {
            self.steps += 1;
            if interp.opcode() == self.opcode {
                interp.set_stop(InstrStop::Revert);
            }
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, TestTypes>) {
            self.step_ends += 1;
        }
    }

    struct StopOnStepEndInspector {
        opcode: u8,
        last_opcode: Option<u8>,
        steps: usize,
        step_ends: usize,
    }

    impl Inspector<TestTypes> for StopOnStepEndInspector {
        fn step(&mut self, interp: &mut Interpreter<'_, TestTypes>) {
            self.steps += 1;
            self.last_opcode = Some(interp.opcode());
        }

        fn step_end(&mut self, interp: &mut Interpreter<'_, TestTypes>) {
            self.step_ends += 1;
            if self.last_opcode == Some(self.opcode) {
                interp.set_stop(InstrStop::Revert);
            }
        }
    }

    #[derive(Default)]
    struct MessageInspector {
        call_depth: Option<u16>,
        call_opcode: Option<u8>,
        call_end_opcode: Option<u8>,
        call_end_stop: Option<InstrStop>,
        create_depth: Option<u16>,
        create_opcode: Option<u8>,
        create_end_opcode: Option<u8>,
        create_end_stop: Option<InstrStop>,
        selfdestruct: Option<(Address, Address, Word)>,
    }

    impl Inspector<TestTypes> for MessageInspector {
        fn call(
            &mut self,
            interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            self.call_depth = Some(message.depth);
            self.call_opcode = Some(interp.opcode());
            None
        }

        fn call_end(
            &mut self,
            interp: &mut Interpreter<'_, TestTypes>,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.call_end_opcode = Some(interp.opcode());
            self.call_end_stop = Some(result.stop);
        }

        fn create(
            &mut self,
            interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            self.create_depth = Some(message.depth);
            self.create_opcode = Some(interp.opcode());
            None
        }

        fn create_end(
            &mut self,
            interp: &mut Interpreter<'_, TestTypes>,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.create_end_opcode = Some(interp.opcode());
            self.create_end_stop = Some(result.stop);
        }

        fn selfdestruct(
            &mut self,
            contract: &Address,
            target: &Address,
            value: &Word,
            _host: &mut TestHost,
        ) {
            self.selfdestruct = Some((*contract, *target, *value));
        }
    }

    struct OverrideCallInspector {
        result: MessageResult<TestTypes>,
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
    }

    impl Inspector<TestTypes> for OverrideCallInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            self.call_depth = Some(message.depth);
            let mut result = self.result.clone();
            result.gas.set_remaining(message.gas_limit);
            Some(result)
        }

        fn call_end(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.call_end_stop = Some(result.stop);
        }
    }

    struct MutateCallInspector {
        destination: Address,
    }

    impl Inspector<TestTypes> for MutateCallInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            message.destination = self.destination;
            None
        }
    }

    struct CallEndInspector;

    impl Inspector<TestTypes> for CallEndInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            Some(MessageResult {
                stop: InstrStop::Revert,
                gas: GasTracker::new(message.gas_limit),
                ..Default::default()
            })
        }

        fn call_end(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            result.stop = InstrStop::Return;
            result.output = Bytes::from_static(&[0xaa, 0xbb]);
        }
    }

    struct OverrideCreateInspector {
        created: Address,
        create_depth: Option<u16>,
        create_end_stop: Option<InstrStop>,
    }

    impl Inspector<TestTypes> for OverrideCreateInspector {
        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            self.create_depth = Some(message.depth);
            Some(MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(message.gas_limit),
                created_address: Some(self.created),
                ..Default::default()
            })
        }

        fn create_end(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.create_end_stop = Some(result.stop);
        }
    }

    #[derive(Default)]
    struct CreateDestinationInspector {
        destination: Option<Address>,
    }

    impl Inspector<TestTypes> for CreateDestinationInspector {
        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            self.destination = Some(message.destination);
            None
        }
    }

    struct CreateEndInspector {
        created: Address,
    }

    impl Inspector<TestTypes> for CreateEndInspector {
        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            message: &mut Message<TestTypes>,
        ) -> Option<MessageResult<TestTypes>> {
            Some(MessageResult {
                stop: InstrStop::Revert,
                gas: GasTracker::new(message.gas_limit),
                ..Default::default()
            })
        }

        fn create_end(
            &mut self,
            _interp: &mut Interpreter<'_, TestTypes>,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            result.stop = InstrStop::Return;
            result.created_address = Some(self.created);
        }
    }

    #[derive(Default)]
    struct LogInspector {
        logs: Vec<Log>,
    }

    impl Inspector<TestTypes> for LogInspector {
        fn log(&mut self, log: &Log, _host: &mut TestHost) {
            self.logs.push(log.clone());
        }
    }

    #[derive(Default)]
    struct FailingStepInspector {
        steps: usize,
        step_ends: usize,
    }

    impl Inspector<TestTypes> for FailingStepInspector {
        fn step(&mut self, _interp: &mut Interpreter<'_, TestTypes>) {
            self.steps += 1;
        }

        fn step_end(&mut self, interp: &mut Interpreter<'_, TestTypes>) {
            let _ = interp;
            self.step_ends += 1;
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
        fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, BaseEvmTypes>) {
            self.state.initialized += 1;
        }

        fn step(&mut self, _interp: &mut Interpreter<'_, BaseEvmTypes>) {
            self.state.steps += 1;
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, BaseEvmTypes>) {
            self.state.step_ends += 1;
        }

        fn log(&mut self, log: &Log, _host: &mut Evm<BaseEvmTypes>) {
            self.state.logs.push(log.clone());
        }

        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, BaseEvmTypes>,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.calls += 1;
            None
        }

        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, BaseEvmTypes>,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.creates += 1;
            None
        }
    }

    fn push_all<const N: usize>(code: &mut Vec<u8>, values: [Word; N]) {
        for value in values {
            push(code, value);
        }
    }

    fn run_with_inspector<I: Inspector<TestTypes>>(
        code: Vec<u8>,
        host: &mut TestHost,
        message: &Message<TestTypes>,
        gas_limit: u64,
        inspector: &mut I,
    ) -> (InstrStop, Vec<Word>) {
        let tx_env = TxEnv::default();
        let bytecode = Bytecode::new_legacy(Bytes::from(code));
        let mut message = message.clone();
        message.gas_limit = gas_limit;
        let mut inner = Interpreter::<TestTypes>::new(bytecode, &tx_env, &message, false);
        let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let stop = inner.run_inspect(&config, host, inspector);
        let stack = inner.stack().to_vec();
        (stop, stack)
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
        let mut host = TestHost::default();
        let mut inspector = StepInspector::default();

        let (stop, _) = run_with_inspector(
            Vec::from([op::STOP]),
            &mut host,
            &Message::default(),
            10_000,
            &mut inspector,
        );

        assert_eq!(stop, InstrStop::Stop);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    #[test]
    fn step_can_stop_before_current_opcode_executes() {
        let mut host = TestHost::default();
        let mut inspector = StopOnStepInspector { opcode: op::ADD, steps: 0, step_ends: 0 };

        let (stop, stack) = run_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &mut host,
            &Message::default(),
            10_000,
            &mut inspector,
        );

        assert_eq!(stop, InstrStop::Revert);
        assert_eq!(stack, [Word::from(1), Word::from(2)]);
        assert_eq!(inspector.steps, 3);
        assert_eq!(inspector.step_ends, 2);
    }

    #[test]
    fn step_end_can_stop_before_next_opcode_executes() {
        let mut host = TestHost::default();
        let mut inspector =
            StopOnStepEndInspector { opcode: op::PUSH1, last_opcode: None, steps: 0, step_ends: 0 };

        let (stop, stack) = run_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &mut host,
            &Message::default(),
            10_000,
            &mut inspector,
        );

        assert_eq!(stop, InstrStop::Revert);
        assert_eq!(stack, [Word::from(1)]);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    #[test]
    fn call_too_deep_is_inspected_without_host_call() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut inspector = MessageInspector::default();
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        let (stop, stack) = run_with_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.call_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.call_opcode, Some(op::CALL));
        assert_eq!(inspector.call_end_opcode, Some(op::CALL));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::CallTooDeep));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_inspector_override_skips_host_and_still_calls_end() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut inspector = OverrideCallInspector {
            result: MessageResult {
                stop: InstrStop::Return,
                output: Bytes::from_static(&[0xaa, 0xbb, 0xcc]),
                ..Default::default()
            },
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL, op::RETURNDATASIZE, op::STOP]);

        let (stop, stack) =
            run_with_inspector(code, &mut host, &Message::default(), 50_000, &mut inspector);

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::from(1), Word::from(3)]);
        assert_eq!(inspector.call_depth, Some(1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::Return));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_inspector_override_wins_at_max_depth() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut inspector = OverrideCallInspector {
            result: MessageResult { stop: InstrStop::Return, ..Default::default() },
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        let (stop, stack) = run_with_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::from(1)]);
        assert_eq!(inspector.call_end_stop, Some(InstrStop::Return));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_inspector_can_mutate_message_before_host() {
        let target = Address::from([0x22; 20]);
        let replacement = Address::from([0x33; 20]);
        let mut host = TestHost::default();
        let mut inspector = MutateCallInspector { destination: replacement };
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        let (stop, stack) =
            run_with_inspector(code, &mut host, &Message::default(), 50_000, &mut inspector);

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::from(1)]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].destination, replacement);
    }

    #[test]
    fn call_end_can_mutate_result_before_opcode_observes_it() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut inspector = CallEndInspector;
        let mut code = call_code(target);
        code.extend([op::CALL, op::RETURNDATASIZE, op::STOP]);

        let (stop, stack) =
            run_with_inspector(code, &mut host, &Message::default(), 50_000, &mut inspector);

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::from(1), Word::from(2)]);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_too_deep_is_inspected_without_host_call() {
        let mut host = TestHost::default();
        let mut inspector = MessageInspector::default();
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (stop, stack) = run_with_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.create_opcode, Some(op::CREATE));
        assert_eq!(inspector.create_end_opcode, Some(op::CREATE));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::CallTooDeep));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_inspector_override_skips_host_and_still_calls_end() {
        let created = Address::from([0x77; 20]);
        let mut host = TestHost::default();
        let mut inspector =
            OverrideCreateInspector { created, create_depth: None, create_end_stop: None };
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (stop, stack) =
            run_with_inspector(code, &mut host, &Message::default(), 50_000, &mut inspector);

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [address_to_word(&created)]);
        assert_eq!(inspector.create_depth, Some(1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_inspector_sees_derived_destination() {
        let contract = Address::from([0x11; 20]);
        let expected = contract.create(0);
        let mut host = TestHost::default();
        let mut inspector = CreateDestinationInspector::default();
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (stop, _) = run_with_inspector(
            code,
            &mut host,
            &Message { destination: contract, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(inspector.destination, Some(expected));
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].destination, expected);
    }

    #[test]
    fn create_inspector_override_wins_at_max_depth() {
        let created = Address::from([0x77; 20]);
        let mut host = TestHost::default();
        let mut inspector =
            OverrideCreateInspector { created, create_depth: None, create_end_stop: None };
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (stop, stack) = run_with_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [address_to_word(&created)]);
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_end_can_mutate_result_before_opcode_observes_it() {
        let created = Address::from([0x88; 20]);
        let mut host = TestHost::default();
        let mut inspector = CreateEndInspector { created };
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (stop, stack) =
            run_with_inspector(code, &mut host, &Message::default(), 50_000, &mut inspector);

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(stack, [address_to_word(&created)]);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn log_opcode_is_inspected_and_emitted_to_host() {
        let contract = Address::from([0x11; 20]);
        let mut host = TestHost::default();
        let mut inspector = LogInspector::default();
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (stop, _) = run_with_inspector(
            code,
            &mut host,
            &Message { destination: contract, ..Default::default() },
            10_000,
            &mut inspector,
        );

        assert_matches!(stop, InstrStop::Stop);
        assert_eq!(inspector.logs.len(), 1);
        assert_eq!(inspector.logs[0].address, contract);
        assert_eq!(host.logs, inspector.logs);
    }

    #[test]
    fn log_opcode_oog_is_not_inspected_or_emitted_to_host() {
        let mut host = TestHost::default();
        let mut inspector = LogInspector::default();
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (stop, _) = run_with_inspector(code, &mut host, &Message::default(), 6, &mut inspector);

        assert_eq!(stop, InstrStop::OutOfGas);
        assert!(inspector.logs.is_empty());
        assert!(host.logs.is_empty());
    }

    #[test]
    fn step_end_runs_for_failing_opcode_with_result_set() {
        let mut host = TestHost::default();
        let mut inspector = FailingStepInspector::default();

        let (stop, _) = run_with_inspector(
            Vec::from([op::INVALID]),
            &mut host,
            &Message::default(),
            10_000,
            &mut inspector,
        );

        assert_eq!(stop, InstrStop::InvalidOpcode);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    #[test]
    fn selfdestruct_is_inspected_from_opcode() {
        let contract = Address::from([0x11; 20]);
        let target = Address::from([0x99; 20]);
        let value = Word::from(0xbeef);
        let mut host = TestHost::default();
        host.selfdestruct_result =
            SelfDestructResult { had_value: true, value, ..Default::default() };
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let (stop, _) = run_with_inspector(
            code,
            &mut host,
            &Message { destination: contract, gas_limit: 10_000, ..Default::default() },
            10_000,
            &mut inspector,
        );

        assert_matches!(stop, InstrStop::SelfDestruct);
        assert_eq!(inspector.selfdestruct, Some((contract, target, value)));
    }

    #[test]
    fn selfdestruct_host_error_is_not_inspected() {
        let target = Address::from([0x99; 20]);
        let mut host = TestHost::default();
        host.selfdestruct_error = Some(InstrStop::FatalExternalError);
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let (stop, _) = run_with_inspector(
            code,
            &mut host,
            &Message { gas_limit: 10_000, ..Default::default() },
            10_000,
            &mut inspector,
        );

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
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { to: TxKind::Call(contract), gas_limit: 100_000, ..Default::default() },
            caller,
        ));

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
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy {
                to: TxKind::Call(target),
                value: U256::from(7),
                gas_limit: 100_000,
                ..Default::default()
            },
            caller,
        ));

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
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy {
                to: TxKind::Create,
                input: Bytes::from_static(&[op::STOP]),
                gas_limit: 100_000,
                ..Default::default()
            },
            caller,
        ));

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
}
