//! EVM execution host.

use self::{
    inspector::Inspector,
    precompile::{PrecompileOutput, PrecompileProvider},
};
use crate::{
    EvmConfigSelector, EvmTypes, ExecutionConfig, PrecompileError, PrecompileHalt, SpecId,
    bytecode::Bytecode,
    constants::{EIP7708_BURN_TOPIC, EIP7708_TRANSFER_TOPIC},
    env::{BlockEnv, TxEnv},
    interpreter::{
        Gas, GasTracker, Host, InstrStop, Interpreter, InterpreterPool, Message, MessageKind,
        MessageResult, Word,
    },
    registry::{HandlerResult, TxRegistry},
    trustme,
    version::{EvmFeatures, GasId},
};
use alloc::{boxed::Box, vec};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Bytes, Log, LogData};

pub mod config;
pub mod env;
pub mod inspector;
pub mod precompile;
pub mod registry;
mod system;
pub use system::{
    BEACON_ROOTS_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS, SYSTEM_ADDRESS,
    SYSTEM_CALL_GAS_LIMIT, WITHDRAWAL_REQUEST_ADDRESS,
};

mod db;
pub use db::{
    Cache, CacheDB, Database, DatabaseCommit, DbErrorCode, DbResult, EmptyDB, InMemoryDB,
};

mod state;
pub use state::{
    Account, AccountInfo, JournalEntry, State, StateChanges, StateCheckpoint, StorageChangeSet,
    StorageOverlay, Tracked,
};

/// EVM host and transaction dispatcher.
#[derive(derive_more::Debug)]
pub struct Evm<T: EvmTypes> {
    #[debug(skip)]
    spec_id: T::SpecId,
    #[debug(skip)]
    execution_config: ExecutionConfig<T>,
    features: EvmFeatures,
    pub(crate) block: BlockEnv,
    registry: TxRegistry<T::Tx, TxResult, Self>,
    #[debug(skip)]
    pub(crate) state: State,
    #[debug(skip)]
    precompiles: Box<dyn PrecompileProvider>,
    #[debug(skip)]
    interpreter_pool: InterpreterPool<T>,
    #[debug(skip)]
    inspector: Option<Box<dyn Inspector<T>>>,
    db_error_code: Option<DbErrorCode>,
}

impl<T: EvmTypes> Evm<T> {
    /// Creates an EVM for `spec_id` with the provided transaction registry, database, and
    /// precompile provider.
    #[inline]
    pub fn new(
        spec_id: T::SpecId,
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: impl Database,
        precompiles: impl PrecompileProvider,
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
        database: impl Database,
        precompiles: impl PrecompileProvider,
    ) -> Self {
        Self::new_mono(
            execution_config,
            spec_id,
            block,
            registry,
            Box::new(database),
            Box::new(precompiles),
        )
    }

    #[inline]
    fn new_mono(
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        block: BlockEnv,
        registry: TxRegistry<T::Tx, TxResult, Self>,
        database: Box<dyn Database>,
        precompiles: Box<dyn PrecompileProvider>,
    ) -> Self {
        assert_eq!(
            spec_id.into(),
            execution_config.version().spec_id,
            "execution config version spec mismatch"
        );
        Self {
            spec_id,
            features: execution_config.version().features,
            execution_config,
            block,
            registry,
            state: State::new_mono(database),
            precompiles,
            interpreter_pool: InterpreterPool::new(),
            inspector: None,
            db_error_code: None,
        }
    }

    #[inline]
    fn execute_precompile(
        &mut self,
        message: &Message,
        gas: &mut Gas,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        if message.disable_precompiles {
            return None;
        }
        self.precompiles.execute(message.code_address, &message.input, gas)
    }
}

impl<T: EvmTypes> Evm<T> {
    /// Returns the transaction handler registry.
    #[inline]
    pub const fn registry(&self) -> &TxRegistry<T::Tx, TxResult, Self> {
        &self.registry
    }

    /// Returns the backing database.
    #[inline]
    pub fn database(&self) -> &dyn Database {
        self.state.initial()
    }

    /// Returns the backing database mutably.
    #[inline]
    pub fn database_mut(&mut self) -> &mut dyn Database {
        self.state.initial_mut()
    }

    /// Returns the latest database error code raised during execution.
    #[inline]
    pub const fn db_error_code(&self) -> Option<DbErrorCode> {
        self.db_error_code
    }

    /// Replaces the backing database.
    #[inline]
    pub fn set_database(&mut self, database: impl Database) {
        self.state.set_initial(database);
    }

    /// Returns the backing database as `D` if it has that concrete type.
    #[inline]
    pub fn database_as<D: Database>(&self) -> Option<&D> {
        <dyn core::any::Any>::downcast_ref(self.database())
    }

    /// Returns the backing database mutably as `D` if it has that concrete type.
    #[inline]
    pub fn database_as_mut<D: Database>(&mut self) -> Option<&mut D> {
        <dyn core::any::Any>::downcast_mut(self.database_mut())
    }

    /// Returns the mutable EVM state.
    #[inline]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Returns logs emitted by the current in-flight transaction.
    #[inline]
    pub fn logs(&self) -> &[Log] {
        self.state.logs()
    }

    /// Returns the precompile provider.
    #[inline]
    pub fn precompiles(&self) -> &dyn PrecompileProvider {
        self.precompiles.as_ref()
    }

    /// Returns the precompile provider mutably.
    #[inline]
    pub fn precompiles_mut(&mut self) -> &mut dyn PrecompileProvider {
        self.precompiles.as_mut()
    }

    /// Replaces the precompile provider.
    #[inline]
    pub fn set_precompiles(&mut self, precompiles: impl PrecompileProvider) {
        self.precompiles = Box::new(precompiles);
    }

    /// Returns the precompile provider as `P` if it has that concrete type.
    #[inline]
    pub fn precompiles_as<P: PrecompileProvider>(&self) -> Option<&P> {
        <dyn core::any::Any>::downcast_ref(self.precompiles())
    }

    /// Returns the precompile provider mutably as `P` if it has that concrete type.
    #[inline]
    pub fn precompiles_as_mut<P: PrecompileProvider>(&mut self) -> Option<&mut P> {
        <dyn core::any::Any>::downcast_mut(self.precompiles_mut())
    }

    /// Returns the active execution inspector.
    #[inline]
    pub fn inspector(&self) -> Option<&dyn Inspector<T>> {
        self.inspector.as_deref()
    }

    /// Returns the active execution inspector mutably.
    #[inline]
    pub fn inspector_mut(&mut self) -> Option<&mut dyn Inspector<T>> {
        self.inspector.as_mut().map(|inspector| inspector.as_mut() as &mut dyn Inspector<T>)
    }

    #[inline]
    fn inspect_log(&mut self, log: &Log) {
        if let Some(inspector) = &mut self.inspector {
            inspector.log(log);
        }
    }

    #[inline]
    fn emit_log(&mut self, log: Log) {
        self.inspect_log(&log);
        self.state.log(log);
    }

    #[inline]
    fn finalize_transaction(&mut self) -> Result<(), InstrStop> {
        self.state
            .finalize_transaction(self.execution_config.version(), |log| {
                if let Some(inspector) = &mut self.inspector {
                    inspector.log(log);
                }
            })
            .map_err(|code| self.db_error_stop(code))
    }

    fn log_eip7708_transfer(&mut self, from: Address, to: Address, value: Word) {
        if self.feature(EvmFeatures::EIP7708)
            && let Some(log) = eip7708_transfer_log(from, to, value)
        {
            self.emit_log(log);
        }
    }

    /// Sets the active execution inspector.
    #[inline]
    pub fn set_inspector<I: Inspector<T> + 'static>(&mut self, inspector: I) {
        self.inspector = Some(Box::new(inspector));
    }

    /// Sets the active boxed execution inspector.
    #[inline]
    pub fn set_boxed_inspector(&mut self, inspector: Box<dyn Inspector<T>>) {
        self.inspector = Some(inspector);
    }

    /// Removes the active execution inspector.
    #[inline]
    pub fn clear_inspector(&mut self) -> Option<Box<dyn Inspector<T>>> {
        self.inspector.take()
    }

    /// Returns the active EVM version.
    #[inline]
    pub const fn version(&self) -> &crate::Version {
        self.execution_config.version()
    }

    /// Returns `true` if the active EVM feature set contains `feature`.
    #[inline]
    pub const fn feature(&self, feature: EvmFeatures) -> bool {
        self.features.contains(feature)
    }

    #[inline]
    const fn db_error_stop(&mut self, code: DbErrorCode) -> InstrStop {
        self.db_error_code = Some(code);
        InstrStop::FatalExternalError
    }

    #[inline]
    pub(crate) const fn db_error_handler(&mut self, code: DbErrorCode) -> registry::HandlerError {
        self.db_error_code = Some(code);
        registry::HandlerError::Database(code)
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.version().spec_id
    }

    /// Returns the selector-specific runtime specification ID.
    #[inline]
    pub const fn config_spec_id(&self) -> T::SpecId {
        self.spec_id
    }
}

impl<T: EvmTypes<Tx: Typed2718>> Evm<T> {
    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte.
    pub fn transact(&mut self, tx: &T::Tx) -> HandlerResult<TxResult> {
        self.db_error_code = None;
        let handler = self.registry.try_get_by_type(tx.ty())?;
        let mut result = handler.call(tx, self);
        if let Ok(result) = &mut result {
            if let Err(stop) = self.finalize_transaction() {
                result.status = false;
                result.stop = stop;
                result.output = Bytes::new();
            } else {
                result.state_changes = self.state.build_state_changes();
                self.state.commit_transaction_overlay();
            }
            result.db_error_code = self.db_error_code;
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
        self.execute_create_message_inner(tx_env, bytecode, message, caller_is_static)
            .unwrap_or_else(|stop| Self::error_message_result(stop, message.gas_limit))
    }

    fn execute_create_message_inner(
        &mut self,
        tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> Result<MessageResult, InstrStop> {
        self.check_create_funds(message)?;
        if message.depth > 0 {
            // EIP-2681 caps account nonces at u64::MAX; CREATE/CREATE2 return zero
            // instead of wrapping or saturating the creator nonce.
            // TODO: Fold this into nonce bumping so account info is not loaded repeatedly.
            if self
                .state
                .account_info(message.caller)
                .map_err(|code| self.db_error_stop(code))?
                .is_some_and(|info| info.nonce == u64::MAX)
            {
                return Err(InstrStop::Return);
            }
        }

        let address = self.create_address(&bytecode, message)?;

        let _ = self.state.warm_account(address);

        if message.depth > 0 {
            self.state.increment_nonce(message.caller).map_err(|code| self.db_error_stop(code))?;
        }

        let checkpoint = self.state.checkpoint();
        if let Err(stop) = self
            .state
            .create_account(message.caller, address, message.value, self.spec_id())
            .map_err(|code| self.db_error_stop(code))?
        {
            self.state.rollback(checkpoint, self.spec_id());
            return Err(stop);
        }
        self.log_eip7708_transfer(message.caller, address, message.value);

        let create_message = Message {
            destination: address,
            code_address: address,
            disable_precompiles: false,
            input: Bytes::new(),

            kind: message.kind,
            depth: message.depth,
            gas_limit: message.gas_limit,
            caller: message.caller,
            value: message.value,
            salt: message.salt,
        };
        let (stop, mut gas, mut output) = {
            let (stop, interpreter) =
                self.run_interpreter(bytecode, tx_env, &create_message, caller_is_static);
            (stop, interpreter.gas(), Bytes::copy_from_slice(interpreter.output()))
        };

        if stop.is_success() {
            if let Err(stop) = self.validate_create_output(&mut gas, &mut output) {
                self.state.rollback(checkpoint, self.spec_id());
                return Ok(MessageResult {
                    stop,
                    gas: Self::message_gas(*gas.tracker(), stop),
                    output,
                    created_address: None,
                });
            }

            self.state
                .set_code(address, Bytecode::new_legacy(output.clone()))
                .map_err(|code| self.db_error_stop(code))?;
        } else {
            self.state.rollback(checkpoint, self.spec_id());
        }

        Ok(MessageResult {
            stop,
            gas: Self::message_gas(*gas.tracker(), stop),
            output,
            created_address: stop.is_success().then_some(address),
        })
    }

    #[inline(never)]
    fn check_create_funds(&mut self, message: &Message) -> Result<(), InstrStop> {
        if message.value > 0
            && self
                .state
                .account_info(message.caller)
                .map_err(|code| self.db_error_stop(code))?
                .is_none_or(|info| info.balance < message.value)
        {
            return Err(InstrStop::OutOfFunds);
        }
        Ok(())
    }

    fn validate_create_output(&self, gas: &mut Gas, output: &mut Bytes) -> Result<(), InstrStop> {
        if self.spec_id().enables(SpecId::SPURIOUS_DRAGON)
            && output.len() > self.version().max_code_size
        {
            return Err(InstrStop::CreateContractSizeLimit);
        }
        if self.feature(EvmFeatures::EIP3541) && output.first().is_some_and(|byte| *byte == 0xef) {
            return Err(InstrStop::CreateContractStartingWithEF);
        }

        let code_deposit_gas = output
            .len()
            .saturating_mul(self.version().gas_params.get(GasId::CodeDepositCost) as usize);
        let code_deposit_gas = u64::try_from(code_deposit_gas).unwrap_or(u64::MAX);
        if gas.remaining() >= code_deposit_gas {
            return gas.spend(code_deposit_gas);
        }
        if self.feature(EvmFeatures::EIP2) {
            // EIP-2 makes code-deposit OOG fail contract creation; Frontier instead creates the
            // account with empty code.
            return Err(InstrStop::OutOfGas);
        }
        *output = Bytes::new();
        Ok(())
    }

    #[inline(never)]
    fn create_address(
        &mut self,
        bytecode: &Bytecode,
        message: &Message,
    ) -> Result<Address, InstrStop> {
        match message.kind {
            MessageKind::Create if message.depth == 0 => Ok(message.destination),
            MessageKind::Create => Ok(message.caller.create(
                self.state
                    .account_info(message.caller)
                    .map_err(|code| self.db_error_stop(code))?
                    .map_or(0, |info| info.nonce),
            )),
            MessageKind::Create2 => Ok(message.caller.create2(message.salt, bytecode.hash_slow())),
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
        self.execute_call_message_inner(tx_env, bytecode, message, caller_is_static)
            .unwrap_or_else(|stop| Self::error_message_result(stop, message.gas_limit))
    }

    fn execute_call_message_inner(
        &mut self,
        tx_env: &TxEnv,
        bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> Result<MessageResult, InstrStop> {
        let checkpoint = self.state.checkpoint();
        // EIP-161 state clearing depends on zero-value direct call targets being touched.
        // CALLCODE also needs the value-transfer balance check.
        let transfers_balance = matches!(
            message.kind,
            MessageKind::Call | MessageKind::CallCode | MessageKind::StaticCall
        );
        let transfer_succeeded = !transfers_balance
            || self
                .state
                .transfer(message.caller, message.destination, message.value)
                .map_err(|code| self.db_error_stop(code))?;
        if transfers_balance && !transfer_succeeded {
            return Err(InstrStop::OutOfFunds);
        }
        if transfers_balance {
            self.log_eip7708_transfer(message.caller, message.destination, message.value);
        }

        let mut gas = Gas::new(message.gas_limit);
        if let Some(result) = self.execute_precompile(message, &mut gas) {
            let (stop, output) = match result {
                Ok(output) => (InstrStop::Return, output.into_bytes()),
                Err(PrecompileError::Revert(output)) => (InstrStop::Revert, output),
                Err(PrecompileError::Halt(PrecompileHalt::OutOfGas)) => {
                    (InstrStop::PrecompileOOG, Bytes::new())
                }
                Err(PrecompileError::Halt(_) | PrecompileError::Fatal(_)) => {
                    (InstrStop::PrecompileError, Bytes::new())
                }
            };
            if !stop.is_success() {
                self.state.rollback(checkpoint, self.spec_id());
            }
            return Ok(MessageResult {
                stop,
                gas: Self::message_gas(*gas.tracker(), stop),
                output,
                created_address: None,
            });
        }

        let (stop, child_gas, output) = {
            let (stop, interpreter) =
                self.run_interpreter(bytecode, tx_env, message, caller_is_static);
            (stop, interpreter.gas(), Bytes::copy_from_slice(interpreter.output()))
        };

        if !stop.is_success() {
            self.state.rollback(checkpoint, self.spec_id());
        }

        Ok(MessageResult {
            stop,
            gas: Self::message_gas(*child_gas.tracker(), stop),
            output,
            created_address: None,
        })
    }

    #[inline]
    fn error_message_result(stop: InstrStop, gas_remaining: u64) -> MessageResult {
        MessageResult {
            stop,
            gas: GasTracker::new(gas_remaining, gas_remaining, 0),
            ..MessageResult::default()
        }
    }

    #[inline]
    const fn message_gas(mut gas: GasTracker, stop: InstrStop) -> GasTracker {
        if stop.is_halt() {
            gas.set_remaining(0);
        }
        if !stop.is_success() {
            gas.set_refunded(0);
        }
        gas
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
        let interpreter_ref = interpreter.as_mut();
        interpreter_ref.init(bytecode, tx_env, message, caller_is_static);
        // SAFETY: `execution_config` points to a private field that host execution does not
        // replace or mutate, so the pointee remains valid here.
        let execution_config = unsafe { trustme::decouple_lt(&self.execution_config) };
        self.inspect_initialize_interp(interpreter_ref);
        let inspector = self.inspector.as_deref_mut().map(|inspector| {
            // SAFETY: The inspector is stored in `self` and remains alive for the duration of the
            // interpreter run.
            unsafe { trustme::decouple_lt_mut(inspector) }
        });
        let stop = if let Some(inspector) = inspector {
            interpreter_ref.run_inspect(execution_config, self, inspector)
        } else {
            interpreter_ref.run(execution_config, self)
        };
        let interpreter = self.interpreter_pool.push(interpreter);
        (stop, interpreter)
    }

    fn inspect_initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        if let Some(inspector) = &mut self.inspector {
            inspector.initialize_interp(interp);
        }
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
        let is_cold = if self.spec_id().enables(SpecId::BERLIN) {
            self.state.warm_account(address)
        } else {
            let _ = self.state.warm_account(address);
            false
        };
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let info = self.state.account_info(address).map_err(|code| self.db_error_stop(code))?;
        let exists = info.is_some();
        let info = info.unwrap_or_default();
        Ok(AccountLoad {
            balance: info.balance,
            code_hash: if exists { info.code_hash } else { B256::ZERO },
            code: if load_code {
                self.state
                    .get_code(address)
                    .map_err(|code| self.db_error_stop(code))?
                    .original_bytes()
            } else {
                Bytes::new()
            },
            exists,
            is_empty: info.is_empty(),
            is_cold,
        })
    }

    fn target_is_empty_for_new_account_gas(
        &mut self,
        address: Address,
        spec: SpecId,
    ) -> Result<bool, InstrStop> {
        self.state
            .target_is_empty_for_new_account_gas(address, spec)
            .map_err(|code| self.db_error_stop(code))
    }

    fn block_hash(&mut self, number: Word) -> Result<Option<B256>, InstrStop> {
        self.state.initial_mut().get_block_hash(number).map_err(|code| self.db_error_stop(code))
    }

    fn sload(
        &mut self,
        address: Address,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop> {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && self.state.warm_storage(address, key);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        Ok(SLoad {
            value: self.state.storage(address, key).map_err(|code| self.db_error_stop(code))?,
            is_cold,
        })
    }

    fn sstore(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop> {
        let is_cold =
            self.spec_id().enables(SpecId::BERLIN) && self.state.warm_storage(address, key);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let mut result =
            self.state.set_storage(address, key, value).map_err(|code| self.db_error_stop(code))?;
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
        let is_cold = if self.spec_id().enables(SpecId::BERLIN) {
            self.state.warm_account(target)
        } else {
            let _ = self.state.warm_account(target);
            false
        };
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let target_is_empty_for_new_account_gas =
            self.target_is_empty_for_new_account_gas(target, self.spec_id())?;
        let previously_destroyed = self.state.is_selfdestructed(contract);
        let balance = self
            .state
            .account_info(contract)
            .map_err(|code| self.db_error_stop(code))?
            .map_or(Word::ZERO, |info| info.balance);
        let should_destroy = !self.spec_id().enables(SpecId::CANCUN)
            || self.state.is_created_in_transaction(contract);

        if contract != target {
            let transferred = self
                .state
                .transfer(contract, target, balance)
                .map_err(|code| self.db_error_stop(code))?;
            if transferred {
                self.log_eip7708_transfer(contract, target, balance);
            }
        } else if should_destroy && !balance.is_zero() {
            if self.feature(EvmFeatures::EIP7708)
                && let Some(log) = eip7708_burn_log(contract, balance)
            {
                self.emit_log(log);
            }
            self.state
                .add_balance(contract, Word::ZERO.wrapping_sub(balance))
                .map_err(|code| self.db_error_stop(code))?;
        }
        if should_destroy {
            self.state.mark_destructed(contract);
        }
        Ok(SelfDestructResult {
            had_value: !balance.is_zero(),
            value: balance,
            target_is_empty: target_is_empty_for_new_account_gas,
            is_cold,
            previously_destroyed,
        })
    }
}

/// Loaded account information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccountLoad {
    /// Account balance.
    pub balance: Word,
    /// Account code hash.
    pub code_hash: B256,
    /// Account code bytes.
    pub code: Bytes,
    /// Whether the account exists in state.
    pub exists: bool,
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
    /// Balance transferred or cleared by the destruction.
    pub value: Word,
    /// Whether the beneficiary is empty/non-existent for new-account gas checks.
    pub target_is_empty: bool,
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
    /// Database error handle, if execution stopped on a database error.
    pub db_error_code: Option<DbErrorCode>,
}

fn eip7708_transfer_log(from: Address, to: Address, value: Word) -> Option<Log> {
    if value.is_zero() || from == to {
        return None;
    }
    let topics = vec![
        EIP7708_TRANSFER_TOPIC,
        B256::left_padding_from(from.as_slice()),
        B256::left_padding_from(to.as_slice()),
    ];
    Some(Log {
        address: SYSTEM_ADDRESS,
        data: LogData::new_unchecked(topics, Bytes::copy_from_slice(&value.to_be_bytes::<32>())),
    })
}

fn eip7708_burn_log(address: Address, value: Word) -> Option<Log> {
    if value.is_zero() {
        return None;
    }
    let topics = vec![EIP7708_BURN_TOPIC, B256::left_padding_from(address.as_slice())];
    Some(Log {
        address: SYSTEM_ADDRESS,
        data: LogData::new_unchecked(topics, Bytes::copy_from_slice(&value.to_be_bytes::<32>())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, Precompiles, SpecId, Version,
        bytecode::Bytecode,
        ethereum::RecoveredTxEnvelope,
        interpreter::{MessageKind, op},
        registry::TxRequest,
    };
    use alloc::{boxed::Box, vec, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, KECCAK256_EMPTY, U256};
    use core::{error::Error, fmt};

    const TEST_TX_TYPE: u8 = 0x00;

    fn test_tx(value: u64) -> RecoveredTxEnvelope {
        RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy { nonce: value, ..TxLegacy::default() },
            Address::ZERO,
        ))
    }

    fn handle_test_tx(
        req: TxRequest<'_, Recovered<TxLegacy>, Evm<BaseEvmTypes>>,
    ) -> HandlerResult<TxResult> {
        let _ = req.host.spec_id();
        Ok(TxResult { status: true, gas_used: req.tx.nonce + 1, ..TxResult::default() })
    }

    fn handle_test_tx_version(
        req: TxRequest<'_, Recovered<TxLegacy>, Evm<BaseEvmTypes>>,
    ) -> HandlerResult<TxResult> {
        Ok(TxResult {
            status: true,
            gas_used: req.host.version().tx_gas_limit_cap,
            ..TxResult::default()
        })
    }

    #[test]
    fn dispatches_transaction_by_typed_2718_type() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_without_evm_config() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA),
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_with_dynamic_version() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            RecoveredTxEnvelope::as_legacy,
            handle_test_tx_version,
        );
        let mut version = crate::Version::new(SpecId::OSAKA);
        version.tx_gas_limit_cap = 42;
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(0);

        assert_eq!(evm.transact(&tx).map(|result| result.gas_used), Ok(42));
    }

    #[test]
    fn dispatches_transaction_iter() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            RecoveredTxEnvelope::as_legacy,
            handle_test_tx,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let txs = [test_tx(1), test_tx(2)];
        let gas_used = evm
            .transact_iter(&txs)
            .map(|result| result.map(|result| result.gas_used))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn host_executes_message() {
        let mut evm = Evm::<BaseEvmTypes>::new(
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

    #[derive(Debug)]
    struct FailingDbError;

    impl fmt::Display for FailingDbError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("storage read failed")
        }
    }

    impl Error for FailingDbError {}

    #[derive(Debug, Default)]
    struct FailingStorageDb;

    impl Database for FailingStorageDb {
        fn get_account(&mut self, _address: Address) -> DbResult<Option<AccountInfo>> {
            Ok(Some(AccountInfo::default()))
        }

        fn get_code_by_hash(&mut self, _code_hash: B256) -> DbResult<Bytecode> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: Address, _key: Word) -> DbResult<Word> {
            Err(DbErrorCode(7))
        }

        fn get_block_hash(&mut self, _number: Word) -> DbResult<Option<B256>> {
            Ok(None)
        }

        fn error(&mut self, code: DbErrorCode) -> Box<dyn Error> {
            assert_eq!(code, DbErrorCode(7));
            Box::new(FailingDbError)
        }
    }

    #[test]
    fn host_records_database_error_code() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            FailingStorageDb,
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::from([0x11; 20]);
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH0, op::SLOAD]));
        let message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &message, false);

        assert_eq!(result.stop, InstrStop::FatalExternalError);
        assert_eq!(evm.db_error_code(), Some(DbErrorCode(7)));
        assert_eq!(evm.database_mut().error(DbErrorCode(7)).to_string(), "storage read failed");
    }

    #[test]
    fn cold_storage_oog_rolls_back_warmth() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::from([0x11; 20]);
        let key = Word::ZERO;
        let bytecode =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0, op::SLOAD, op::STOP]));
        let message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 500,
            ..Message::default()
        };

        let result = Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &message, false);

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert!(!evm.state.is_storage_warm(contract, key));
    }

    #[test]
    fn frontier_code_deposit_oog_creates_empty_contract() {
        let caller = Address::from([0x11; 20]);
        let created = Address::from([0x22; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(caller, AccountInfo::default().with_balance(Word::from(1)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::FRONTIER,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::FRONTIER),
        );
        let message = Message {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50,
            ..Message::default()
        };
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 1, op::PUSH1, 0, op::RETURN]));

        let result = Host::execute_message(&mut evm, &TxEnv::default(), code, &message, false);
        assert!(result.stop.is_success());

        evm.state.finalize_transaction_(Version::base(SpecId::FRONTIER));
        let changes = evm.state.build_state_changes();
        let account =
            changes.accounts.get(&created).and_then(|change| change.current.as_ref()).unwrap();
        assert_eq!(account.code_hash, KECCAK256_EMPTY);
    }

    #[test]
    fn homestead_code_deposit_oog_fails_create() {
        let caller = Address::from([0x11; 20]);
        let created = Address::from([0x22; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(caller, AccountInfo::default().with_balance(Word::from(1)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::HOMESTEAD,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::HOMESTEAD),
        );
        let message = Message {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50,
            ..Message::default()
        };
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 1, op::PUSH1, 0, op::RETURN]));

        let result = Host::execute_message(&mut evm, &TxEnv::default(), code, &message, false);
        assert_eq!(result.stop, InstrStop::OutOfGas);

        evm.state.finalize_transaction_(Version::base(SpecId::HOMESTEAD));
        let changes = evm.state.build_state_changes();
        assert!(!changes.accounts.contains_key(&created));
    }

    #[test]
    fn staticcall_touches_empty_existing_destination() {
        let target = Address::from([0x11; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(target, AccountInfo::default());
        database.insert_account_storage(target, Word::ZERO, Word::from(1));
        let mut evm = Evm::<BaseEvmTypes>::new(
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

        evm.state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
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
        let mut evm = Evm::<BaseEvmTypes>::new(
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

        evm.state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
        let changes = evm.state.build_state_changes();
        assert!(!changes.accounts.contains_key(&code_address));
        assert!(!changes.storage.contains_key(&code_address));
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
        state.add_balance(from, U256::from(10)).unwrap();

        assert!(state.transfer(from, to, U256::from(7)).unwrap());
        assert_eq!(
            state.account_info(from).expect("sender account should exist").unwrap().balance,
            U256::from(3)
        );
        assert_eq!(
            state.account_info(to).expect("recipient account should exist").unwrap().balance,
            U256::from(7)
        );
    }

    #[test]
    fn amsterdam_call_value_emits_eip7708_log() {
        let caller = Address::from([0x01; 20]);
        let target = Address::from([0x02; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(caller, AccountInfo::default().with_balance(U256::from(10)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        let message = Message {
            kind: MessageKind::Call,
            destination: target,
            caller,
            value: U256::from(7),
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

        let version = *evm.version();
        evm.state.finalize_transaction_(&version);
        let changes = evm.state.build_state_changes();
        assert_eq!(changes.logs.len(), 1);
        let log = &changes.logs[0];
        assert_eq!(log.address, SYSTEM_ADDRESS);
        assert_eq!(
            log.topics(),
            &[
                EIP7708_TRANSFER_TOPIC,
                B256::left_padding_from(caller.as_slice()),
                B256::left_padding_from(target.as_slice()),
            ]
        );
        assert_eq!(log.data.data, Bytes::copy_from_slice(&U256::from(7).to_be_bytes::<32>()));
    }
}
