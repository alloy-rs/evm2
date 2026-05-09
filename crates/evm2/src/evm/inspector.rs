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
    fn log(&mut self, log: &Log) {
        let _ = log;
    }

    /// Called before a call message executes.
    #[inline]
    fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
        let _ = message;
        None
    }

    /// Called after a call message executes.
    #[inline]
    fn call_end(&mut self, message: &Message, result: &mut MessageResult) {
        let _ = message;
        let _ = result;
    }

    /// Called before a create message executes.
    #[inline]
    fn create(&mut self, message: &mut Message) -> Option<MessageResult> {
        let _ = message;
        None
    }

    /// Called after a create message executes.
    #[inline]
    fn create_end(&mut self, message: &Message, result: &mut MessageResult) {
        let _ = message;
        let _ = result;
    }

    /// Called after a contract self-destructs.
    #[inline]
    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        let _ = contract;
        let _ = target;
        let _ = value;
    }
}

#[cfg(test)]
mod tests {
    use super::Inspector;
    use crate::{
        BaseEvmConfigSelector, ExecutionConfig, SpecId,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::TxEnv,
        evm::SelfDestructResult,
        interpreter::{
            InstrStop, Interpreter, Message, MessageResult, Word,
            instructions::tests::{TestHost, TestTypes, push},
            op,
        },
        utils::address_to_word,
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, Bytes};

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

    #[derive(Default)]
    struct MessageInspector {
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
        create_depth: Option<u16>,
        create_end_stop: Option<InstrStop>,
        selfdestruct: Option<(Address, Address, Word)>,
    }

    impl Inspector<TestTypes> for MessageInspector {
        fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
            self.call_depth = Some(message.depth);
            None
        }

        fn call_end(&mut self, _message: &Message, result: &mut MessageResult) {
            self.call_end_stop = Some(result.stop);
        }

        fn create(&mut self, message: &mut Message) -> Option<MessageResult> {
            self.create_depth = Some(message.depth);
            None
        }

        fn create_end(&mut self, _message: &Message, result: &mut MessageResult) {
            self.create_end_stop = Some(result.stop);
        }

        fn selfdestruct(&mut self, contract: Address, target: Address, value: Word) {
            self.selfdestruct = Some((contract, target, value));
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
        message: &Message,
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
    fn call_too_deep_is_inspected_without_host_call() {
        let target = Address::from([0x22; 20]);
        let mut host = TestHost::default();
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                address_to_word(target),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let (stop, stack) = run_with_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.call_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::CallTooDeep));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_too_deep_is_inspected_without_host_call() {
        let mut host = TestHost::default();
        let mut inspector = MessageInspector::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let (stop, stack) = run_with_inspector(
            code,
            &mut host,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::CallTooDeep));
        assert!(host.calls.is_empty());
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
        push(&mut code, address_to_word(target));
        code.push(op::SELFDESTRUCT);

        let (stop, _) = run_with_inspector(
            code,
            &mut host,
            &Message { destination: contract, gas_limit: 10_000, ..Default::default() },
            10_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::SelfDestruct));
        assert_eq!(inspector.selfdestruct, Some((contract, target, value)));
    }
}
