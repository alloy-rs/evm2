//! EVM execution host.

use self::precompile::{PrecompileOutput, PrecompileProvider};
use crate::{
    EvmConfigSelector, EvmTypes, ExecutionConfig, PrecompileError, PrecompileHalt, SpecId,
    bytecode::Bytecode,
    constants::{CALL_DEPTH_LIMIT, MAX_CODE_SIZE},
    env::{BlockEnv, TxEnv},
    interpreter::{
        Gas, Host, InstrStop, Interpreter, InterpreterPool, Message, MessageKind, MessageResult,
        Word,
    },
    registry::{HandlerResult, TxRegistry},
    version::GasId,
};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Bytes, Log};

pub mod config;
pub mod env;
pub mod precompile;
pub mod registry;

mod db;
pub use db::{Cache, CacheDB, Database, EmptyDB, InMemoryDB};

mod state;
pub use state::{
    Account, AccountInfo, JournalEntry, State, StateChanges, StorageChangeSet, StorageOverlay,
    Tracked,
};

/// Loaded account information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccountLoad {
    /// Account balance.
    pub balance: Word,
    /// Account code hash.
    pub code_hash: B256,
    /// Account code bytes.
    pub code: Bytes,
    /// Whether the account is empty.
    pub is_empty: bool,
    /// Whether the account access was cold.
    pub is_cold: bool,
}

/// Result of an `SLOAD` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SLoad {
    /// Storage slot value.
    pub value: Word,
    /// Whether the storage slot access was cold.
    pub is_cold: bool,
}

/// Result of an `SSTORE` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SStore {
    /// Storage value at the start of the transaction.
    pub original_value: Word,
    /// Storage value immediately before this `SSTORE`.
    pub present_value: Word,
    /// Storage value written by this `SSTORE`.
    pub new_value: Word,
    /// Whether the storage slot access was cold.
    pub is_cold: bool,
}

impl SStore {
    /// Returns whether this `SSTORE` leaves the slot unchanged (`new == present`).
    #[inline]
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.new_value == self.present_value
    }

    /// Returns whether the slot is clean (`original == present`).
    #[inline]
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.original_value == self.present_value
    }

    /// Returns whether this `SSTORE` restores the slot to its original value (`new == original`).
    #[inline]
    #[must_use]
    pub fn resets_original(&self) -> bool {
        self.original_value == self.new_value
    }

    /// Returns whether the original value is zero.
    #[inline]
    #[must_use]
    pub fn original_is_zero(&self) -> bool {
        self.original_value.is_zero()
    }

    /// Returns whether the present value is zero.
    #[inline]
    #[must_use]
    pub fn present_is_zero(&self) -> bool {
        self.present_value.is_zero()
    }

    /// Returns whether the new value is zero.
    #[inline]
    #[must_use]
    pub fn new_is_zero(&self) -> bool {
        self.new_value.is_zero()
    }
}

/// Result of a `SELFDESTRUCT` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SelfDestructResult {
    /// Whether the destroyed account had non-zero value.
    pub had_value: bool,
    /// Whether the beneficiary account already exists.
    pub target_exists: bool,
    /// Whether the beneficiary access was cold.
    pub is_cold: bool,
    /// Whether this account was already destroyed in this transaction.
    pub previously_destroyed: bool,
}

/// Result of executing a transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxResult {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas used by execution.
    pub gas_used: u64,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
    /// State transition and logs produced by this transaction.
    pub state_changes: StateChanges,
}

/// EVM host and transaction dispatcher.
#[derive(derive_more::Debug)]
pub struct Evm<T: EvmTypes> {
    #[debug(skip)]
    spec_id: T::SpecId,
    #[debug(skip)]
    execution_config: ExecutionConfig<T>,
    pub(crate) block: BlockEnv,
    registry: TxRegistry<T::Tx, TxResult, Self>,
    pub(crate) state: State<T::Database>,
    precompiles: T::Precompiles,
    #[debug(skip)]
    interpreter_pool: InterpreterPool<T>,
}

impl<T: EvmTypes> Evm<T> {
    /// Creates an EVM for `spec_id` with the provided transaction registry, database, and
    /// precompile provider.
    #[inline]
    pub fn new(
        spec_id: T::SpecId,
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: T::Database,
        precompiles: T::Precompiles,
    ) -> Self {
        Self::new_with_execution_config(
            <T::ConfigSelector as EvmConfigSelector<T>>::execution_config(spec_id),
            spec_id,
            block,
            registry,
            database,
            precompiles,
        )
    }

    /// Creates an EVM with the provided transaction registry, database, and precompile provider.
    #[inline]
    pub fn new_with_execution_config(
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: T::Database,
        precompiles: T::Precompiles,
    ) -> Self {
        Self {
            spec_id,
            execution_config,
            block,
            registry,
            state: State::new(database),
            precompiles,
            interpreter_pool: InterpreterPool::new(),
        }
    }

    #[inline]
    fn execute_precompile(
        &mut self,
        message: &Message,
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        self.precompiles.execute(message.code_address, &message.input, gas)
    }
}

impl<T: EvmTypes> Evm<T> {
    /// Returns the transaction handler registry.
    pub const fn registry(&self) -> &TxRegistry<T::Tx, TxResult, Self> {
        &self.registry
    }

    /// Returns the backing database.
    pub const fn database(&self) -> &State<T::Database> {
        &self.state
    }

    /// Returns the mutable EVM state.
    pub const fn state(&self) -> &State<T::Database> {
        &self.state
    }

    /// Returns logs emitted by the current in-flight transaction.
    pub fn logs(&self) -> &[Log] {
        self.state.logs()
    }

    /// Returns the precompile provider.
    pub const fn precompiles(&self) -> &T::Precompiles {
        &self.precompiles
    }

    /// Returns the precompile provider mutably.
    pub const fn precompiles_mut(&mut self) -> &mut T::Precompiles {
        &mut self.precompiles
    }

    /// Returns the active EVM version.
    pub const fn version(&self) -> &crate::Version {
        self.execution_config.version()
    }

    /// Returns the active base specification ID.
    pub const fn spec_id(&self) -> SpecId {
        self.version().spec_id()
    }

    /// Returns the selector-specific runtime specification ID.
    pub const fn config_spec_id(&self) -> T::SpecId {
        self.spec_id
    }
}

impl<T: EvmTypes<Tx: Typed2718>> Evm<T> {
    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte.
    pub fn transact(&mut self, tx: &T::Tx) -> HandlerResult<TxResult> {
        let handler = self.registry.try_get_by_type(tx.ty())?;
        let mut result = handler.call(tx, self);
        if let Ok(result) = &mut result {
            self.state.finalize_transaction(self.spec_id());
            result.state_changes = self.state.build_state_changes();
            self.state.commit_transaction_overlay();
        };
        self.state.clear_transaction_state();
        result
    }

    /// Dispatches each transaction to its registered EIP-2718 handler.
    pub fn transact_iter<'a, I>(
        &'a mut self,
        txs: I,
    ) -> impl Iterator<Item = HandlerResult<TxResult>> + 'a
    where
        I: IntoIterator<Item = &'a T::Tx>,
        I::IntoIter: 'a,
        T::Tx: 'a,
        Self: 'a,
    {
        txs.into_iter().map(move |tx| self.transact(tx))
    }
}

impl<T: EvmTypes<Host = Self>> Evm<T> {
    #[inline(never)]
    fn execute_create_message(
        &mut self,
        tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> MessageResult {
        if let Err(stop) = self.check_create_funds(message) {
            return MessageResult {
                stop,
                gas_remaining: message.gas_limit,
                ..MessageResult::default()
            };
        }

        let address = self.create_address(&bytecode, message);

        self.state.warm_account(address);

        if message.depth > 0 {
            self.state.increment_nonce(message.caller);
        }

        let checkpoint = self.state.checkpoint();
        if let Err(stop) =
            self.state.create_account(message.caller, address, message.value, self.spec_id())
        {
            self.state.rollback(checkpoint);
            return MessageResult {
                stop,
                gas_remaining: message.gas_limit,
                ..MessageResult::default()
            };
        }

        let create_message = Message {
            destination: address,
            code_address: address,
            input: Bytes::new(),

            kind: message.kind,
            depth: message.depth,
            gas_limit: message.gas_limit,
            caller: message.caller,
            value: message.value,
            salt: message.salt,
        };
        let (stop, mut gas, output) = {
            let (stop, interpreter) =
                self.run_interpreter(bytecode, tx_env, &create_message, caller_is_static);
            (stop, interpreter.gas(), Bytes::copy_from_slice(interpreter.output()))
        };
        let mut gas_remaining = gas.remaining();
        let mut gas_refunded = if stop.is_success() { gas.refunded() } else { 0 };

        if stop.is_success() {
            let stop = if self.spec_id().enables(SpecId::SPURIOUS_DRAGON)
                && output.len() > MAX_CODE_SIZE
            {
                Some(InstrStop::CreateContractSizeLimit)
            } else if self.spec_id().enables(SpecId::LONDON)
                && output.first().is_some_and(|byte| *byte == 0xef)
            {
                Some(InstrStop::CreateContractStartingWithEF)
            } else {
                let code_deposit_gas = output.len().saturating_mul(
                    self.version().gas_params().get(GasId::CodeDepositCost) as usize,
                );
                gas.spend(u64::try_from(code_deposit_gas).unwrap_or(u64::MAX)).err()
            };

            if let Some(stop) = stop {
                self.state.rollback(checkpoint);
                gas_remaining = if stop.is_halt() { 0 } else { gas.remaining() };
                return MessageResult {
                    stop,
                    gas_remaining,
                    gas_refunded: 0,
                    output,
                    created_address: None,
                };
            }

            gas_remaining = gas.remaining();
            gas_refunded = gas.refunded();
            self.state.set_code(address, Bytecode::new_legacy(output.clone()));
        } else {
            self.state.rollback(checkpoint);
            if stop.is_halt() {
                gas_remaining = 0;
            }
        }

        MessageResult {
            stop,
            gas_remaining,
            gas_refunded: if stop.is_success() { gas_refunded } else { 0 },
            output,
            created_address: stop.is_success().then_some(address),
        }
    }

    #[inline(never)]
    fn check_create_funds(&mut self, message: &Message) -> Result<(), InstrStop> {
        if message.value > 0
            && self
                .state
                .account_info(message.caller)
                .is_none_or(|info| info.balance < message.value)
        {
            return Err(InstrStop::OutOfFunds);
        }
        Ok(())
    }

    #[inline(never)]
    fn create_address(&mut self, bytecode: &Bytecode, message: &Message) -> Address {
        match message.kind {
            MessageKind::Create if message.depth == 0 => message.destination,
            MessageKind::Create => message
                .caller
                .create(self.state.account_info(message.caller).map_or(0, |info| info.nonce)),
            MessageKind::Create2 => message.caller.create2(message.salt, bytecode.hash_slow()),
            _ => unreachable!("invalid create message kind"),
        }
    }

    #[inline(never)]
    fn execute_call_message(
        &mut self,
        tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> MessageResult {
        let checkpoint = self.state.checkpoint();
        // EIP-161 state clearing depends on zero-value direct call targets being touched.
        if matches!(message.kind, MessageKind::Call | MessageKind::StaticCall)
            && !self.state.transfer(message.caller, message.destination, message.value)
        {
            return MessageResult {
                stop: InstrStop::OutOfFunds,
                gas_remaining: message.gas_limit,
                ..MessageResult::default()
            };
        }

        let mut gas = Gas::new(message.gas_limit);
        if let Some(result) = self.execute_precompile(message, &mut gas) {
            let (stop, gas_remaining, output) = match result {
                Ok(output) => (InstrStop::Return, gas.remaining(), output.into_bytes()),
                Err(PrecompileError::Revert(output)) => {
                    (InstrStop::Revert, gas.remaining(), output)
                }
                Err(PrecompileError::Halt(PrecompileHalt::OutOfGas)) => {
                    (InstrStop::PrecompileOOG, 0, Bytes::new())
                }
                Err(PrecompileError::Halt(_) | PrecompileError::Fatal(_)) => {
                    let stop = InstrStop::PrecompileError;
                    let gas_remaining = if stop.is_halt() { 0 } else { gas.remaining() };
                    (stop, gas_remaining, Bytes::new())
                }
            };
            if !stop.is_success() {
                self.state.rollback(checkpoint);
            }
            return MessageResult {
                stop,
                gas_remaining,
                gas_refunded: 0,
                output,
                created_address: None,
            };
        }

        let (stop, child_gas, output) = {
            let (stop, interpreter) =
                self.run_interpreter(bytecode, tx_env, message, caller_is_static);
            (stop, interpreter.gas(), Bytes::copy_from_slice(interpreter.output()))
        };
        let mut gas_remaining = child_gas.remaining();

        if !stop.is_success() {
            self.state.rollback(checkpoint);
            if stop.is_halt() {
                gas_remaining = 0;
            }
        }

        MessageResult {
            stop,
            gas_remaining,
            gas_refunded: if stop.is_success() { child_gas.refunded() } else { 0 },
            output,
            created_address: None,
        }
    }

    #[inline(never)]
    fn run_interpreter<'frame>(
        &mut self,
        bytecode: Bytecode,
        tx_env: &'frame TxEnv,
        message: &'frame Message,
        caller_is_static: bool,
    ) -> (InstrStop, &mut Interpreter<'frame, T>) {
        let mut interpreter = self.interpreter_pool.pop();
        // SAFETY: The active interpreter is owned by this stack frame until it is returned to the
        // pool after execution. Recursive calls may mutate the pool, but they cannot move this box.
        let interpreter_ref = unsafe { &mut *(&mut *interpreter as *mut Interpreter<'frame, T>) };
        interpreter_ref.init(bytecode, tx_env, message, caller_is_static);
        let stop = interpreter_ref.run_with(self.execution_config, self);
        let interpreter = self.interpreter_pool.push(interpreter);
        (stop, interpreter)
    }
}

impl<T: EvmTypes<Host = Self>> Host for Evm<T> {
    fn spec_id(&self) -> SpecId {
        self.spec_id()
    }

    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && !self.state.is_account_warm(address);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        self.state.warm_account(address);
        let info = self.state.account_info(address).unwrap_or_default();
        Ok(AccountLoad {
            balance: info.balance,
            code_hash: if info.is_empty() { B256::ZERO } else { info.code_hash },
            code: if load_code {
                self.state.get_code(address).original_bytes()
            } else {
                Bytes::new()
            },
            is_empty: info.is_empty(),
            is_cold,
        })
    }

    fn block_hash(&mut self, number: Word) -> Option<B256> {
        self.state.initial_mut().get_block_hash(number)
    }

    fn sload(
        &mut self,
        address: Address,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop> {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && !self.state.is_storage_warm(address, key);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        if is_cold {
            let _ = self.state.warm_storage(address, key);
        }
        Ok(SLoad { value: self.state.storage(address, key), is_cold })
    }

    fn sstore(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop> {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && !self.state.is_storage_warm(address, key);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        if is_cold {
            let _ = self.state.warm_storage(address, key);
        }
        let mut result = self.state.set_storage(address, key, value);
        result.is_cold = is_cold;
        Ok(result)
    }

    fn tload(&mut self, address: Address, key: Word) -> Word {
        self.state.transient_storage(address, key)
    }

    fn tstore(&mut self, address: Address, key: Word, value: Word) {
        self.state.set_transient_storage(address, key, value);
    }

    fn log(&mut self, log: Log) {
        self.state.log(log);
    }

    fn execute_message(
        &mut self,
        tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> MessageResult {
        if message.depth > CALL_DEPTH_LIMIT {
            return MessageResult {
                stop: InstrStop::CallTooDeep,
                gas_remaining: message.gas_limit,
                ..MessageResult::default()
            };
        }

        match message.kind {
            MessageKind::Create | MessageKind::Create2 => {
                self.execute_create_message(tx_env, bytecode, message, caller_is_static)
            }
            MessageKind::Call
            | MessageKind::CallCode
            | MessageKind::DelegateCall
            | MessageKind::StaticCall => {
                self.execute_call_message(tx_env, bytecode, message, caller_is_static)
            }
        }
    }

    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        // TODO: evmone applies full SELFDESTRUCT revision rules in state transition.
        let is_cold = self.spec_id().enables(SpecId::BERLIN) && !self.state.is_account_warm(target);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        self.state.warm_account(target);
        // EIP-161 changes account emptiness semantics: before Spurious Dragon, an empty
        // account that exists in state is still an existing `SELFDESTRUCT` beneficiary and
        // must not be charged EIP-150's new-account topup.
        let target_exists = if self.spec_id().enables(SpecId::SPURIOUS_DRAGON) {
            self.state.account_info(target).is_some_and(|info| !info.is_empty())
        } else {
            self.state.account_info(target).is_some()
        };
        let previously_destroyed = self.state.is_selfdestructed(contract);
        let balance = self.state.account_info(contract).map_or(Word::ZERO, |info| info.balance);
        let should_destroy = !self.spec_id().enables(SpecId::CANCUN)
            || self.state.is_created_in_transaction(contract);

        if contract != target {
            let _ = self.state.transfer(contract, target, balance);
        } else if should_destroy && !balance.is_zero() {
            self.state.add_balance(contract, Word::ZERO.wrapping_sub(balance));
        }
        if should_destroy {
            self.state.mark_destructed(contract);
        }

        Ok(SelfDestructResult {
            had_value: !balance.is_zero(),
            target_exists,
            is_cold,
            previously_destroyed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfig, BaseEvmTypes, Precompiles, SpecId,
        bytecode::Bytecode,
        interpreter::{MessageKind, op},
        registry::TxRequest,
    };
    use alloy_primitives::{Address, Bytes, U256};

    const TEST_TX_TYPE: u8 = 0x7f;

    #[derive(Debug)]
    struct TestTx {
        value: u64,
    }

    type TestEvmTypes<Tx = ()> = BaseEvmTypes<Tx>;

    const NO_CONFIG_EXECUTION: ExecutionConfig<TestEvmTypes<TestTx>> =
        ExecutionConfig::for_config::<BaseEvmConfig<{ SpecId::OSAKA as u8 }>>();

    impl Typed2718 for TestTx {
        fn ty(&self) -> u8 {
            TEST_TX_TYPE
        }
    }

    fn extract_test_tx(tx: &TestTx) -> Option<&TestTx> {
        Some(tx)
    }

    fn handle_test_tx(
        req: TxRequest<'_, TestTx, Evm<TestEvmTypes<TestTx>>>,
    ) -> HandlerResult<TxResult> {
        let _ = req.host.spec_id();
        Ok(TxResult { status: true, gas_used: req.tx.value + 1, ..TxResult::default() })
    }

    #[test]
    fn dispatches_transaction_by_typed_2718_type() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let mut evm = Evm::<TestEvmTypes<TestTx>>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = TestTx { value: 41 };

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_without_evm_config() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let mut evm = Evm::<TestEvmTypes<TestTx>>::new_with_execution_config(
            NO_CONFIG_EXECUTION,
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = TestTx { value: 41 };

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_iter() {
        let registry =
            TxRegistry::new().with_handler(TEST_TX_TYPE, extract_test_tx, handle_test_tx);
        let mut evm = Evm::<TestEvmTypes<TestTx>>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let txs = [TestTx { value: 1 }, TestTx { value: 2 }];
        let gas_used = evm
            .transact_iter(&txs)
            .map(|result| result.map(|result| result.gas_used))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn host_executes_message() {
        let mut evm = Evm::<TestEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::from([0x11; 20]);
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::ADDRESS, op::STOP]));
        let message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &message, false);
        assert!(result.stop.is_success());
    }

    #[test]
    fn staticcall_touches_empty_existing_destination() {
        let target = Address::from([0x11; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(target, AccountInfo::default());
        database.insert_account_storage(target, Word::ZERO, Word::from(1));
        let mut evm = Evm::<TestEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let message = Message {
            kind: MessageKind::StaticCall,
            destination: target,
            code_address: target,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(
            &mut evm,
            &TxEnv::default(),
            Bytecode::default(),
            &message,
            false,
        );
        assert!(result.stop.is_success());

        evm.state.finalize_transaction(SpecId::SPURIOUS_DRAGON);
        let changes = evm.state.build_state_changes();
        let account = changes.accounts.get(&target).expect("empty destination should be deleted");
        assert!(account.original.is_some());
        assert_eq!(account.current, None);
        assert!(changes.storage.get(&target).is_some_and(|storage| storage.wipe));
    }

    #[test]
    fn delegatecall_does_not_touch_empty_code_address() {
        let destination = Address::from([0x11; 20]);
        let code_address = Address::from([0x22; 20]);
        let mut database = InMemoryDB::default();
        database
            .insert_account_info(destination, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_info(code_address, AccountInfo::default());
        database.insert_account_storage(code_address, Word::ZERO, Word::from(1));
        let mut evm = Evm::<TestEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let message = Message {
            kind: MessageKind::DelegateCall,
            destination,
            code_address,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(
            &mut evm,
            &TxEnv::default(),
            Bytecode::default(),
            &message,
            false,
        );
        assert!(result.stop.is_success());

        evm.state.finalize_transaction(SpecId::SPURIOUS_DRAGON);
        let changes = evm.state.build_state_changes();
        assert!(!changes.accounts.contains_key(&code_address));
        assert!(!changes.storage.contains_key(&code_address));
    }

    #[test]
    fn host_allows_message_at_call_depth_limit() {
        let mut evm = Evm::<TestEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let message = Message {
            kind: MessageKind::Call,
            depth: CALL_DEPTH_LIMIT,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &message, false);
        assert!(result.stop.is_success());
    }

    #[test]
    fn host_rejects_message_past_call_depth_limit() {
        let mut evm = Evm::<TestEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let message = Message {
            kind: MessageKind::Call,
            depth: CALL_DEPTH_LIMIT + 1,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &message, false);
        assert_eq!(result.stop, InstrStop::CallTooDeep);
        assert_eq!(result.gas_remaining, 50_000);
    }

    #[test]
    fn account_info_with_code_sets_hash() {
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let info = AccountInfo::default().with_code(code.clone());

        assert_eq!(info.code_hash, code.hash_slow());
    }

    #[test]
    fn transfer_moves_value() {
        let from = Address::from([0x01; 20]);
        let to = Address::from([0x02; 20]);
        let mut state = State::new(InMemoryDB::default());
        state.add_balance(from, U256::from(10));

        assert!(state.transfer(from, to, U256::from(7)));
        assert_eq!(
            state.account_info(from).expect("sender account should exist").balance,
            U256::from(3)
        );
        assert_eq!(
            state.account_info(to).expect("recipient account should exist").balance,
            U256::from(7)
        );
    }
}
