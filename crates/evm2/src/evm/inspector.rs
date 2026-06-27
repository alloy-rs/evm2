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
pub trait Inspector<T: EvmTypes>: Any {
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
    fn log(&mut self, log: &Log, host: &mut T::Host) {
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
    ///
    /// The interpreter is the currently running frame whose instruction produced the message; for
    /// the top-level message it is a frame initialized with the message itself.
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
    use super::{Inspector, InspectorConfig, OpcodeSet};
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmTypes, ExecutionConfig, Precompiles, SpecId,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::{BlockEnv, TxEnv},
        ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, InMemoryDB, SYSTEM_ADDRESS},
        interpreter::{GasTracker, Host, InstrStop, Interpreter, Message, MessageResult, Word, op},
        registry::TxRegistry,
        test_utils::{TestHost, TestTypes, legacy_bytecode, push, push_all},
        utils::address_to_word,
    };
    use alloc::{boxed::Box, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
    use core::{assert_matches, marker::PhantomData};

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

    struct OverrideCallInspector {
        result: MessageResult<BaseEvmTypes>,
        min_depth: u16,
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for OverrideCallInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, BaseEvmTypes>,
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
            _interp: &mut Interpreter<'_, BaseEvmTypes>,
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
            _interp: &mut Interpreter<'_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
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
            _interp: &mut Interpreter<'_, BaseEvmTypes>,
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

    impl<T: EvmTypes> Inspector<T> for LogInspector {
        fn log(&mut self, log: &Log, _host: &mut T::Host) {
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

    fn run_evm_with_inspector_db<I: Inspector<BaseEvmTypes> + 'static>(
        db: InMemoryDB,
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<BaseEvmTypes>) {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(inspector);
        let tx_env = TxEnv::default();
        let bytecode = legacy_bytecode(code);
        let mut message = message.clone();
        message.gas_limit = gas_limit;
        let result = Host::execute_message(&mut evm, &tx_env, bytecode, &mut message);
        let inspector = evm.clear_inspector_as::<I>().unwrap();
        (result, inspector, evm)
    }

    fn run_with_inspector<I: Inspector<TestTypes>>(
        code: Vec<u8>,
        host: &mut TestHost,
        message: &Message<TestTypes>,
        gas_limit: u64,
        inspector: &mut I,
    ) -> (InstrStop, Vec<Word>) {
        let tx_env = TxEnv::default();
        let bytecode = legacy_bytecode(code);
        let mut message = message.clone();
        message.gas_limit = gas_limit;
        let mut interp = Interpreter::<TestTypes>::new(bytecode, &tx_env, &message);
        let mut config =
            ExecutionConfig::<TestTypes>::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let inspector_config = inspector.config();
        config.register_inspector(&inspector_config);
        let stop = interp.run_inspect(&config, &inspector_config, host, inspector);
        let stack = interp.stack().to_vec();
        (stop, stack)
    }

    #[derive(Default)]
    struct SelfdestructInspector {
        selfdestruct: Option<(Address, Address, Word)>,
    }

    impl<T: EvmTypes> Inspector<T> for SelfdestructInspector {
        fn selfdestruct(
            &mut self,
            contract: &Address,
            target: &Address,
            value: &Word,
            _host: &mut T::Host,
        ) {
            self.selfdestruct = Some((*contract, *target, *value));
        }
    }

    #[derive(Default)]
    struct HookInspector {
        call_depths: Vec<u16>,
        call_opcode: Option<u8>,
        call_end_opcode: Option<u8>,
        call_end_stops: Vec<InstrStop>,
        create_depths: Vec<u16>,
        create_opcode: Option<u8>,
        create_destinations: Vec<Address>,
        create_end_opcode: Option<u8>,
        create_end_stops: Vec<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for HookInspector {
        fn call(
            &mut self,
            interp: &mut Interpreter<'_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.call_depths.push(message.depth);
            self.call_opcode = Some(interp.opcode());
            None
        }

        fn call_end(
            &mut self,
            interp: &mut Interpreter<'_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.call_end_opcode = Some(interp.opcode());
            self.call_end_stops.push(result.stop);
        }

        fn create(
            &mut self,
            interp: &mut Interpreter<'_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.create_depths.push(message.depth);
            self.create_opcode = Some(interp.opcode());
            self.create_destinations.push(message.destination);
            None
        }

        fn create_end(
            &mut self,
            interp: &mut Interpreter<'_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.create_end_opcode = Some(interp.opcode());
            self.create_end_stops.push(result.stop);
        }
    }

    fn run_evm_with_inspector<I: Inspector<BaseEvmTypes> + 'static>(
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<BaseEvmTypes>) {
        run_evm_with_inspector_db(InMemoryDB::default(), code, message, gas_limit, inspector)
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

        impl<T: EvmTypes> Inspector<T> for StepInspector {
            fn step(&mut self, _interp: &mut Interpreter<'_, T>) {
                self.steps += 1;
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, T>) {
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::STOP]),
            &Message::default(),
            10_000,
            StepInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
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

        impl<T: EvmTypes> Inspector<T> for StopOnStepInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, T>) {
                self.steps += 1;
                if interp.opcode() == self.opcode {
                    self.stack = interp.stack().to_vec();
                    interp.set_stop(InstrStop::Revert);
                }
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, T>) {
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &Message::default(),
            10_000,
            StopOnStepInspector { opcode: op::ADD, ..Default::default() },
        );

        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(inspector.stack, [Word::from(1), Word::from(2)]);
        assert_eq!(inspector.steps, 3);
        assert_eq!(inspector.step_ends, 2);
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

        impl<T: EvmTypes> Inspector<T> for StopOnStepEndInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, T>) {
                self.steps += 1;
                self.last_opcode = Some(interp.opcode());
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
                self.step_ends += 1;
                if self.last_opcode == Some(self.opcode) {
                    self.stack = interp.stack().to_vec();
                    interp.set_stop(InstrStop::Revert);
                }
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &Message::default(),
            10_000,
            StopOnStepEndInspector { opcode: op::PUSH1, ..Default::default() },
        );

        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(inspector.stack, [Word::from(1)]);
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
    fn call_too_deep_is_inspected_without_executing() {
        let target = Address::from([0x22; 20]);
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.call_depths, [CALL_DEPTH_LIMIT, CALL_DEPTH_LIMIT + 1]);
        assert_eq!(inspector.call_opcode, Some(op::CALL));
        assert_eq!(inspector.call_end_opcode, Some(op::CALL));
        assert_eq!(inspector.call_end_stops, [InstrStop::CallTooDeep, InstrStop::Stop]);
    }

    #[test]
    fn call_inspector_override_skips_execution_and_still_calls_end() {
        let target = Address::from([0x22; 20]);
        let inspector = OverrideCallInspector {
            result: MessageResult {
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
            run_evm_with_inspector(code, &Message::default(), 50_000, inspector);

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
            result: MessageResult { stop: InstrStop::Return, ..Default::default() },
            min_depth: CALL_DEPTH_LIMIT + 1,
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL]);
        return_top_word(&mut code);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
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
                _interp: &mut Interpreter<'_, BaseEvmTypes>,
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
            BlockEnv::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(MutateCallInspector { destination: replacement });
        let tx_env = TxEnv::default();
        let bytecode = legacy_bytecode(code);
        let mut message = Message { gas_limit: 100_000, ..Default::default() };
        let result = Host::execute_message(&mut evm, &tx_env, bytecode, &mut message);

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
                _interp: &mut Interpreter<'_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 0 {
                    return None;
                }
                Some(MessageResult {
                    stop: InstrStop::Revert,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                _interp: &mut Interpreter<'_, BaseEvmTypes>,
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
            run_evm_with_inspector(code, &Message::default(), 50_000, CallEndInspector);

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
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.create_depths, [CALL_DEPTH_LIMIT + 1]);
        assert_eq!(inspector.create_opcode, Some(op::CREATE));
        assert_eq!(inspector.create_end_opcode, Some(op::CREATE));
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
            run_evm_with_inspector(code, &Message::default(), 50_000, inspector);

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
            &Message { destination: contract, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.create_destinations, [expected]);
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
            &Message { depth: CALL_DEPTH_LIMIT, ..Default::default() },
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
                _interp: &mut Interpreter<'_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                Some(MessageResult {
                    stop: InstrStop::Revert,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn create_end(
                &mut self,
                _interp: &mut Interpreter<'_, BaseEvmTypes>,
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
            &Message::default(),
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
            &Message { destination: contract, ..Default::default() },
            10_000,
            LogInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.logs.len(), 1);
        assert_eq!(inspector.logs[0].address, contract);
        assert_eq!(evm.logs(), inspector.logs);
    }

    #[test]
    fn empty_opcode_set_skips_steps_but_keeps_other_hooks() {
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

            fn log(&mut self, log: &Log, _host: &mut TestHost) {
                self.logs.push(log.clone());
            }
        }

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
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (result, inspector, evm) =
            run_evm_with_inspector(code, &Message::default(), 6, LogInspector::default());

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

        impl<T: EvmTypes> Inspector<T> for FailingStepInspector {
            fn step(&mut self, _interp: &mut Interpreter<'_, T>) {
                self.steps += 1;
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
                let _ = interp;
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::INVALID]),
            &Message::default(),
            10_000,
            FailingStepInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::InvalidOpcode);
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
            &Message { destination: contract, ..Default::default() },
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
            &Message { destination: contract, ..Default::default() },
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

        let tx_env = TxEnv::default();
        let bytecode = legacy_bytecode(code);
        let message = Message::<TestTypes> { gas_limit: 10_000, ..Default::default() };
        let mut interp = Interpreter::<TestTypes>::new(bytecode, &tx_env, &message);
        let mut config =
            ExecutionConfig::<TestTypes>::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let inspector_config = <SelfdestructInspector as Inspector<TestTypes>>::config(&inspector);
        config.register_inspector(&inspector_config);
        let stop = interp.run_inspect(&config, &inspector_config, &mut host, &mut inspector);

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

        let result = evm.transact(&tx).unwrap().discard();
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
    fn evm_transaction_reconfigures_inspector_for_nested_frame() {
        const CHEATCODE_ADDRESS: Address = Address::repeat_byte(0x71);

        struct FakeCheatcodesInspector {
            set: OpcodeSet,
            steps: usize,
            opcodes: Vec<u8>,
            cheatcode_calls: usize,
        }

        impl Default for FakeCheatcodesInspector {
            fn default() -> Self {
                let mut set = OpcodeSet::EMPTY;
                set.insert(op::CALL);
                Self { set, steps: 0, opcodes: Vec::new(), cheatcode_calls: 0 }
            }
        }

        impl Inspector<BaseEvmTypes> for FakeCheatcodesInspector {
            fn config(&self) -> InspectorConfig {
                InspectorConfig::new().with_opcode_set(self.set)
            }

            fn step(&mut self, interp: &mut Interpreter<'_, BaseEvmTypes>) {
                self.steps += 1;
                self.opcodes.push(interp.opcode());
            }

            fn call(
                &mut self,
                interp: &mut Interpreter<'_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.destination != CHEATCODE_ADDRESS {
                    return None;
                }
                self.cheatcode_calls += 1;
                self.set.insert(op::SLOAD);
                interp.request_inspector_reconfigure();
                Some(MessageResult {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }
        }

        let caller = Address::from([0xaa; 20]);
        let contract = Address::from([0xbb; 20]);
        let child = Address::from([0xcc; 20]);
        let mut parent_code = call_code(CHEATCODE_ADDRESS);
        parent_code.push(op::CALL);
        parent_code.extend(call_code(child));
        parent_code.extend([op::CALL, op::STOP]);
        let parent_code = Bytecode::new_legacy(Bytes::from(parent_code));
        let child_code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0, op::SLOAD, op::STOP]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        database.insert_account_info(&contract, AccountInfo::default().with_code(parent_code));
        database.insert_account_info(&child, AccountInfo::default().with_code(child_code));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(FakeCheatcodesInspector::default());
        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { to: TxKind::Call(contract), gas_limit: 100_000, ..Default::default() },
            caller,
        ));

        let result = evm.transact(&tx).unwrap().discard();
        let inspector = evm.inspector().unwrap().downcast_ref::<FakeCheatcodesInspector>().unwrap();

        assert!(result.status);
        assert_eq!(inspector.cheatcode_calls, 1);
        assert_eq!(inspector.steps, 3);
        assert_eq!(inspector.opcodes, [op::CALL, op::CALL, op::SLOAD]);
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
