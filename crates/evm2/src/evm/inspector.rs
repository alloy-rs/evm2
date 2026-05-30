//! EVM execution inspection hooks.

use crate::{
    EvmTypes,
    interpreter::{Interpreter, Message, MessageResult},
};
use alloy_primitives::{Address, Log, U256};
use core::any::Any;

/// Set of opcodes an inspector wants step hooks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct OpcodeSet(U256);

impl OpcodeSet {
    /// Empty opcode set.
    pub const EMPTY: Self = Self(U256::ZERO);

    /// Set containing every opcode.
    pub const ALL: Self = Self(U256::MAX);

    /// Creates an opcode set from raw bits.
    #[inline]
    pub const fn new(bits: U256) -> Self {
        Self(bits)
    }

    /// Returns the raw opcode set bits.
    #[inline]
    pub const fn get(&self) -> U256 {
        self.0
    }

    /// Returns an iterator over enabled opcodes.
    #[inline]
    pub const fn bits(&self) -> OpcodeSetBits {
        OpcodeSetBits { bits: self.0 }
    }

    /// Returns whether this set contains `opcode`.
    #[inline]
    pub const fn contains(&self, opcode: u8) -> bool {
        self.0.bit(opcode as usize)
    }

    /// Inserts `opcode` into this set.
    #[inline]
    pub const fn insert(&mut self, opcode: u8) {
        self.0.set_bit(opcode as usize, true);
    }

    /// Returns whether all opcodes in `other` are also in this set.
    #[inline]
    pub fn contains_set(&self, other: &Self) -> bool {
        self.intersection(other).get() == other.get()
    }

    /// Returns whether this set and `other` share any opcodes.
    #[inline]
    pub fn intersects(&self, other: &Self) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Returns whether this set contains no opcodes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_zero()
    }

    /// Removes `opcode` from this set.
    #[inline]
    pub const fn remove(&mut self, opcode: u8) {
        self.0.set_bit(opcode as usize, false);
    }

    /// Returns the union of this set and `other`.
    #[inline]
    pub fn union(&self, other: &Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns the intersection of this set and `other`.
    #[inline]
    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Returns opcodes present in this set but not in `other`.
    #[inline]
    pub fn difference(&self, other: &Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Returns opcodes present in exactly one of the two sets.
    #[inline]
    pub fn symmetric_difference(&self, other: &Self) -> Self {
        Self(self.0 ^ other.0)
    }
}

/// Iterator over enabled opcodes in an [`OpcodeSet`].
#[derive(Clone, Copy, Debug)]
pub struct OpcodeSetBits {
    bits: U256,
}

impl Iterator for OpcodeSetBits {
    type Item = u8;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.bits.is_zero() {
            return None;
        }
        let bit = self.bits.trailing_zeros();
        self.bits.set_bit(bit, false);
        Some(bit as u8)
    }
}

/// Execution inspection configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectorConfig {
    /// Set of opcodes for which step hooks are enabled.
    pub set: OpcodeSet,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl InspectorConfig {
    /// Creates an inspector configuration.
    #[inline]
    pub const fn new() -> Self {
        Self { set: OpcodeSet::ALL, _non_exhaustive: () }
    }

    /// Sets the opcodes for which step hooks are enabled.
    #[inline]
    pub const fn with_opcode_set(mut self, set: OpcodeSet) -> Self {
        self.set = set;
        self
    }
}

impl Default for InspectorConfig {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// EVM execution inspector.
pub trait Inspector<T: EvmTypes>: Any + Send {
    /// Returns this inspector's execution configuration.
    #[inline]
    fn config(&self) -> InspectorConfig {
        InspectorConfig::new()
    }

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
    fn call(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        let _ = message;
        None
    }

    /// Called after a call message executes.
    #[inline]
    fn call_end(&mut self, message: &Message<T>, result: &mut MessageResult<T>) {
        let _ = message;
        let _ = result;
    }

    /// Called before a create message executes.
    #[inline]
    fn create(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        let _ = message;
        None
    }

    /// Called after a create message executes.
    #[inline]
    fn create_end(&mut self, message: &Message<T>, result: &mut MessageResult<T>) {
        let _ = message;
        let _ = result;
    }

    /// Called after a contract self-destructs.
    #[inline]
    fn selfdestruct(&mut self, contract: &Address, target: &Address, value: &U256) {
        let _ = contract;
        let _ = target;
        let _ = value;
    }
}

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
    use super::{Inspector, InspectorConfig, OpcodeSet};
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmTypes, ExecutionConfig, Precompiles, SpecId,
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
    use core::marker::PhantomData;

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

    struct OpcodeInterestInspector<T: EvmTypes> {
        steps: usize,
        step_ends: usize,
        opcodes: Vec<u8>,
        _marker: PhantomData<fn() -> T>,
    }

    impl<T: EvmTypes> Default for OpcodeInterestInspector<T> {
        fn default() -> Self {
            Self { steps: 0, step_ends: 0, opcodes: Vec::new(), _marker: PhantomData }
        }
    }

    impl<T: EvmTypes> Inspector<T> for OpcodeInterestInspector<T> {
        fn config(&self) -> InspectorConfig {
            let mut set = OpcodeSet::EMPTY;
            set.insert(op::ADD);
            InspectorConfig::new().with_opcode_set(set)
        }

        fn step(&mut self, interp: &mut Interpreter<'_, T>) {
            self.steps += 1;
            self.opcodes.push(interp.opcode());
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, T>) {
            self.step_ends += 1;
        }
    }

    struct ReconfiguringInspector {
        set: OpcodeSet,
        steps: usize,
        opcodes: Vec<u8>,
    }

    impl Default for ReconfiguringInspector {
        fn default() -> Self {
            Self { set: OpcodeSet::EMPTY, steps: 0, opcodes: Vec::new() }
        }
    }

    impl Inspector<BaseEvmTypes> for ReconfiguringInspector {
        fn config(&self) -> InspectorConfig {
            InspectorConfig::new().with_opcode_set(self.set)
        }

        fn initialize_interp(&mut self, interp: &mut Interpreter<'_, BaseEvmTypes>) {
            self.set.insert(op::ADD);
            interp.request_inspector_reconfigure();
        }

        fn step(&mut self, interp: &mut Interpreter<'_, BaseEvmTypes>) {
            self.steps += 1;
            self.opcodes.push(interp.opcode());
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
        fn call(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
            self.call_depth = Some(message.depth);
            None
        }

        fn call_end(
            &mut self,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.call_end_stop = Some(result.stop);
        }

        fn create(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
            self.create_depth = Some(message.depth);
            None
        }

        fn create_end(
            &mut self,
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.create_end_stop = Some(result.stop);
        }

        fn selfdestruct(&mut self, contract: &Address, target: &Address, value: &Word) {
            self.selfdestruct = Some((*contract, *target, *value));
        }
    }

    struct OverrideCallInspector {
        result: MessageResult<TestTypes>,
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
    }

    impl Inspector<TestTypes> for OverrideCallInspector {
        fn call(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
            self.call_depth = Some(message.depth);
            let mut result = self.result.clone();
            result.gas.set_remaining(message.gas_limit);
            Some(result)
        }

        fn call_end(
            &mut self,
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
        fn call(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
            message.destination = self.destination;
            None
        }
    }

    struct CallEndInspector;

    impl Inspector<TestTypes> for CallEndInspector {
        fn call(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
            Some(MessageResult {
                stop: InstrStop::Revert,
                gas: GasTracker::new(message.gas_limit),
                ..Default::default()
            })
        }

        fn call_end(
            &mut self,
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
        fn create(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
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
            _message: &Message<TestTypes>,
            result: &mut MessageResult<TestTypes>,
        ) {
            self.create_end_stop = Some(result.stop);
        }
    }

    struct CreateEndInspector {
        created: Address,
    }

    impl Inspector<TestTypes> for CreateEndInspector {
        fn create(&mut self, message: &mut Message<TestTypes>) -> Option<MessageResult<TestTypes>> {
            Some(MessageResult {
                stop: InstrStop::Revert,
                gas: GasTracker::new(message.gas_limit),
                ..Default::default()
            })
        }

        fn create_end(
            &mut self,
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
        fn log(&mut self, log: &Log) {
            self.logs.push(log.clone());
        }
    }

    #[derive(Default)]
    struct EmptySetLogInspector {
        steps: usize,
        step_ends: usize,
        logs: Vec<Log>,
    }

    impl Inspector<TestTypes> for EmptySetLogInspector {
        fn config(&self) -> InspectorConfig {
            InspectorConfig::new().with_opcode_set(OpcodeSet::EMPTY)
        }

        fn step(&mut self, _interp: &mut Interpreter<'_, TestTypes>) {
            self.steps += 1;
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, TestTypes>) {
            self.step_ends += 1;
        }

        fn log(&mut self, log: &Log) {
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

        fn log(&mut self, log: &Log) {
            self.state.logs.push(log.clone());
        }

        fn call(
            &mut self,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.calls += 1;
            None
        }

        fn create(
            &mut self,
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
        let mut config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let inspector_config = inspector.config();
        config.register_inspector(&inspector_config);
        let stop = inner.run_inspect(&config, &inspector_config, host, inspector);
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
    fn inspector_only_steps_interested_opcodes() {
        let mut host = TestHost::default();
        let mut inspector = OpcodeInterestInspector::default();

        let (stop, stack) = run_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &mut host,
            &Message::default(),
            10_000,
            &mut inspector,
        );

        assert_eq!(stop, InstrStop::Stop);
        assert_eq!(stack, [Word::from(3)]);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
        assert_eq!(inspector.opcodes, [op::ADD]);
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

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.call_depth, Some(CALL_DEPTH_LIMIT + 1));
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

        assert!(matches!(stop, InstrStop::Stop));
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

        assert!(matches!(stop, InstrStop::Stop));
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

        assert!(matches!(stop, InstrStop::Stop));
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

        assert!(matches!(stop, InstrStop::Stop));
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

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [Word::ZERO]);
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
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

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(stack, [address_to_word(&created)]);
        assert_eq!(inspector.create_depth, Some(1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
        assert!(host.calls.is_empty());
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

        assert!(matches!(stop, InstrStop::Stop));
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

        assert!(matches!(stop, InstrStop::Stop));
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

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(inspector.logs.len(), 1);
        assert_eq!(inspector.logs[0].address, contract);
        assert_eq!(host.logs, inspector.logs);
    }

    #[test]
    fn empty_opcode_set_skips_steps_but_keeps_other_hooks() {
        let contract = Address::from([0x11; 20]);
        let mut host = TestHost::default();
        let mut inspector = EmptySetLogInspector::default();
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (stop, _) = run_with_inspector(
            code,
            &mut host,
            &Message { destination: contract, ..Default::default() },
            10_000,
            &mut inspector,
        );

        assert!(matches!(stop, InstrStop::Stop));
        assert_eq!(inspector.steps, 0);
        assert_eq!(inspector.step_ends, 0);
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

        assert!(matches!(stop, InstrStop::SelfDestruct));
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

        let result = evm.transact(&tx).unwrap();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.status);
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 4);
        assert_eq!(state.step_ends, 4);
        assert_eq!(state.logs.len(), 1);
        assert_eq!(state.logs[0].address, contract);
        assert_eq!(state.calls, 0);
        assert_eq!(state.creates, 0);
    }

    #[test]
    fn evm_transaction_registers_inspector_opcode_interest() {
        let caller = Address::from([0xaa; 20]);
        let contract = Address::from([0xbb; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            1,
            op::PUSH1,
            2,
            op::ADD,
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
        evm.set_inspector(OpcodeInterestInspector::<BaseEvmTypes>::default());
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { to: TxKind::Call(contract), gas_limit: 100_000, ..Default::default() },
            caller,
        ));

        let result = evm.transact(&tx).unwrap();
        let inspector = evm
            .inspector()
            .unwrap()
            .downcast_ref::<OpcodeInterestInspector<BaseEvmTypes>>()
            .unwrap();

        assert!(result.status);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
        assert_eq!(inspector.opcodes, [op::ADD]);
    }

    #[test]
    fn evm_transaction_reconfigures_inspector_from_initialize() {
        let caller = Address::from([0xaa; 20]);
        let contract = Address::from([0xbb; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            1,
            op::PUSH1,
            2,
            op::ADD,
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
        evm.set_inspector(ReconfiguringInspector::default());
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { to: TxKind::Call(contract), gas_limit: 100_000, ..Default::default() },
            caller,
        ));

        let result = evm.transact(&tx).unwrap();
        let inspector = evm.inspector().unwrap().downcast_ref::<ReconfiguringInspector>().unwrap();

        assert!(result.status);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.opcodes, [op::ADD]);
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

        let result = evm.transact(&tx).unwrap();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.status);
        assert_eq!(result.state_changes.logs.len(), 1);
        assert_eq!(state.logs, result.state_changes.logs);
        assert_eq!(state.logs[0].address, SYSTEM_ADDRESS);
    }

    #[test]
    fn evm_create_transaction_initializes_interpreter_without_create_hook() {
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

        let result = evm.transact(&tx).unwrap();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.status);
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 1);
        assert_eq!(state.step_ends, 1);
        assert_eq!(state.calls, 0);
        assert_eq!(state.creates, 0);
    }
}
