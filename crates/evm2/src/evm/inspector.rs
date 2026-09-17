//! EVM execution inspection hooks.

use crate::{
    EvmTypesHost,
    evm::NonStaticAny,
    interpreter::{Interpreter, Message, MessageResult},
};
use alloc::sync::Arc;
use alloy_primitives::{Address, Log, U256};
use auto_impl::auto_impl;
use core::cell::RefCell;

/// EVM execution inspector.
///
/// [`crate::Evm::set_inspector`] adapts this trait through [`RefCell`]. Ordinary nested
/// EVM calls remain inspected, but reentry while a hook is still running panics.
/// Implement [`SharedInspector`] for callbacks that invoke the host recursively.
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
    /// Opcode calls borrow the parent interpreter. Direct host calls use a separate frame
    /// initialized with a snapshot of the message.
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
    /// Opcode calls borrow the parent interpreter. Direct host calls use a separate frame
    /// initialized with a snapshot of the message.
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

/// Reentrant EVM execution inspector.
///
/// Mutable state must use interior mutability. Release state borrows before invoking
/// the host recursively. Ordinary inspectors can use [`Inspector`] instead.
#[auto_impl(&, &mut, Box)]
pub trait SharedInspector<T: EvmTypesHost>: NonStaticAny {
    /// Called after a frame interpreter has been initialized.
    #[inline]
    fn initialize_interp(&self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called before each instruction executes.
    #[inline]
    fn step(&self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called after each instruction executes.
    #[inline]
    fn step_end(&self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called when a log is emitted.
    #[inline]
    fn log(&self, log: &Log, host: &mut T::Host<'_>) {
        let _ = log;
        let _ = host;
    }

    /// Called before a call message executes.
    ///
    /// Opcode calls borrow the parent interpreter. Direct host calls use a separate frame
    /// initialized with a snapshot of the message.
    #[inline]
    fn call(
        &self,
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
        &self,
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
    /// Opcode calls borrow the parent interpreter. Direct host calls use a separate frame
    /// initialized with a snapshot of the message.
    #[inline]
    fn create(
        &self,
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
        &self,
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
        &self,
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

impl<T: EvmTypesHost, I: Inspector<T>> SharedInspector<T> for RefCell<I> {
    fn initialize_interp(&self, interp: &mut Interpreter<'_, '_, T>) {
        self.borrow_mut().initialize_interp(interp)
    }

    fn step(&self, interp: &mut Interpreter<'_, '_, T>) {
        self.borrow_mut().step(interp)
    }

    fn step_end(&self, interp: &mut Interpreter<'_, '_, T>) {
        self.borrow_mut().step_end(interp)
    }

    fn log(&self, log: &Log, host: &mut T::Host<'_>) {
        self.borrow_mut().log(log, host)
    }

    fn call(
        &self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.borrow_mut().call(interp, message)
    }

    fn call_end(
        &self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.borrow_mut().call_end(interp, message, result)
    }

    fn create(
        &self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.borrow_mut().create(interp, message)
    }

    fn create_end(
        &self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.borrow_mut().create_end(interp, message, result)
    }

    fn selfdestruct(
        &self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        self.borrow_mut().selfdestruct(contract, target, value, host)
    }
}

#[inline]
pub(crate) fn shared_inspector<'a, T: EvmTypesHost>(
    inspector: impl Inspector<T> + 'a,
) -> Arc<dyn SharedInspector<T> + 'a> {
    Arc::new(RefCell::new(inspector))
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

impl<'a, T: EvmTypesHost> core::ops::Deref for dyn SharedInspector<T> + 'a {
    type Target = dyn NonStaticAny + 'a;

    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a, T: EvmTypesHost> core::ops::DerefMut for dyn SharedInspector<T> + 'a {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmTypesHost, ExecutionConfig, Precompiles,
        SpecId,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::{BlockEnvExt, TxEnvExt},
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, InMemoryDB, SYSTEM_ADDRESS},
        interpreter::{
            GasTracker, Host, InstrStop, Interpreter, Message, MessageExt, MessageResult,
            MessageResultExt, Word, op,
        },
        registry::TxRegistry,
        test_utils::{TestHost, TestTypes, legacy_bytecode, push, push_all},
        utils::address_to_word,
    };
    use alloc::{boxed::Box, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
    use core::{assert_matches, cell::Cell};
    use std::panic::{AssertUnwindSafe, catch_unwind};

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
    fn shared_inspector_can_reenter_from_step() {
        struct RecursiveInspector {
            steps: Cell<usize>,
            depths: RefCell<Vec<u16>>,
        }

        impl SharedInspector<BaseEvmTypes> for RecursiveInspector {
            fn step(&self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                self.steps.set(self.steps.get() + 1);
                if interp.message().depth == 0 {
                    let tx = interp.tx_env();
                    let mut child = MessageExt {
                        depth: 1,
                        gas_limit: 100,
                        code: legacy_bytecode([op::STOP]),
                        ..Default::default()
                    };
                    assert_eq!(interp.host().execute_message(tx, &mut child).stop, InstrStop::Stop);
                }
            }

            fn call(
                &self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                self.depths.borrow_mut().push(message.depth);
                None
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_shared_inspector(RecursiveInspector {
            steps: Cell::new(0),
            depths: RefCell::new(Vec::new()),
        });
        let mut message =
            MessageExt { gas_limit: 100, code: legacy_bytecode([op::STOP]), ..Default::default() };
        assert_eq!(evm.execute_message(&TxEnvExt::default(), &mut message).stop, InstrStop::Stop);
        let inspector = evm.inspector().unwrap().downcast_ref::<RecursiveInspector>().unwrap();
        assert_eq!(inspector.steps.get(), 2);
        assert_eq!(*inspector.depths.borrow(), [0, 1]);
    }

    #[test]
    fn mutable_inspector_reentry_panics_and_releases_borrow() {
        struct RecursiveInspector;

        impl Inspector<BaseEvmTypes> for RecursiveInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let tx = interp.tx_env();
                let mut child = MessageExt {
                    depth: 1,
                    gas_limit: 100,
                    code: legacy_bytecode([op::STOP]),
                    ..Default::default()
                };
                let _ = interp.host().execute_message(tx, &mut child);
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(RecursiveInspector);
        let mut message =
            MessageExt { gas_limit: 100, code: legacy_bytecode([op::STOP]), ..Default::default() };
        assert!(
            catch_unwind(AssertUnwindSafe(
                || evm.execute_message(&TxEnvExt::default(), &mut message)
            ))
            .is_err()
        );
        assert!(evm.clear_inspector_as::<RecursiveInspector>().is_some());
        evm.set_inspector(NoopInspector::default());
        assert_eq!(evm.execute_message(&TxEnvExt::default(), &mut message).stop, InstrStop::Stop);
    }

    #[test]
    fn nested_call_hooks_borrow_the_parent_frame() {
        #[derive(Default)]
        struct ParentInspector {
            pcs: Vec<usize>,
        }

        impl Inspector<BaseEvmTypes> for ParentInspector {
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 1 {
                    assert_eq!(interp.message().depth, 0);
                    assert_eq!(interp.opcode(), op::CALL);
                    self.pcs.push(interp.pc());
                    // Host access must remain independent of the parent-frame borrow.
                    assert_eq!(interp.host().spec_id(), SpecId::OSAKA);
                }
                None
            }

            fn call_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                _result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.depth == 1 {
                    assert_eq!(interp.pc(), self.pcs[0]);
                    interp.memory_mut().resize(0, 1).unwrap();
                    interp.memory_mut().set(0, b"!");
                }
            }
        }

        let mut code = call_code(Address::with_last_byte(0xff));
        code.extend([op::CALL, op::POP, op::PUSH1, 1, op::PUSH0, op::RETURN]);
        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt::default(),
            50_000,
            ParentInspector::default(),
        );
        assert_eq!(result.stop, InstrStop::Return);
        assert_eq!(result.output.as_ref(), b"!");
        assert_eq!(inspector.pcs.len(), 1);
    }

    #[test]
    fn interpreter_clears_host_access_on_return_and_unwind() {
        let tx = TxEnvExt::default();
        let message =
            Message::<TestTypes> { code: legacy_bytecode([op::STOP]), ..Default::default() };
        let mut interp = Interpreter::<TestTypes>::new(&tx, &message);
        let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let mut host = TestHost::default();
        assert_eq!(interp.run(&config, &mut host), InstrStop::Stop);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = interp.host();
            }))
            .is_err()
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = interp.version();
            }))
            .is_err()
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                interp.with_host(config.base_spec_id(), config.version(), &mut host, |_| {
                    panic!("hook panic")
                });
            }))
            .is_err()
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = interp.host();
            }))
            .is_err()
        );
    }

    #[test]
    fn nested_host_scope_restores_inspection_after_unwind() {
        #[derive(Default)]
        struct NestedScopeInspector {
            step_ends: usize,
        }

        impl Inspector<TestTypes> for NestedScopeInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, TestTypes>) {
                let mut inner_host = TestHost::default();
                let inner_version = crate::Version::new(SpecId::BERLIN);
                assert!(
                    catch_unwind(AssertUnwindSafe(|| {
                        interp.with_host(
                            SpecId::BERLIN,
                            &inner_version,
                            &mut inner_host,
                            |inner| {
                                assert_eq!(inner.spec(), SpecId::BERLIN);
                                panic!("nested scope");
                            },
                        );
                    }))
                    .is_err()
                );
                assert_eq!(interp.spec(), SpecId::OSAKA);
                assert_eq!(interp.version().features, crate::Version::new(SpecId::OSAKA).features);
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, TestTypes>) {
                self.step_ends += 1;
            }
        }

        let tx = TxEnvExt::default();
        let message =
            Message::<TestTypes> { code: legacy_bytecode([op::STOP]), ..Default::default() };
        let mut interp = Interpreter::<TestTypes>::new(&tx, &message);
        let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA);
        let mut host = TestHost::default();
        let mut inspector = NestedScopeInspector::default();
        assert_eq!(interp.run_inspect(&config, &mut host, &mut inspector), InstrStop::Stop);
        assert_eq!(inspector.step_ends, 1);
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
        let inspector = evm.inspector_as::<SharedE2eInspector>().unwrap();
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
        let inspector = evm.inspector_as::<SharedE2eInspector>().unwrap();
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
        let inspector = evm.inspector_as::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.status);
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 1);
        assert_eq!(state.step_ends, 1);
        assert_eq!(state.calls, 0);
        assert_eq!(state.creates, 1);
    }
}
