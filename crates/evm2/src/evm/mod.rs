//! EVM execution host.
//!
//! See [`state-output.md`](../../docs/state-output.md) for the transaction lifecycle and
//! state-output model behind `transact`, `commit`, `commit_to`, `commit_with`, `discard`, and
//! `detach`.

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
use alloc::{boxed::Box, vec, vec::Vec};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Bytes, Log, LogData};
#[cfg(feature = "async")]
use core::future::Future;
use core::{any::TypeId, fmt};
use derive_where::derive_where;

#[cfg(feature = "async")]
pub mod r#async;
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
    AccountStorageCache, Cache, CacheDB, CacheDbSink, Database, DatabaseCommit, Db, DbErrorCode,
    DbResult, DynDatabase, EmptyDB, InMemoryDB,
};
#[cfg(feature = "async")]
pub(crate) use db::{db_error_unavailable, stored_error_code};

mod state;
pub use state::{
    Account, AccountChangeRef, AccountInfo, AccountInfoRef, BlockAccountDelta,
    BlockStateAccumulator, BlockStorageDelta, FrozenBlockState, JournalEntry, NoopChangeSink,
    State, StateChanges, StateCheckpoint, StorageChangeRef, StorageChangeSet, StorageOverlay, Tee,
    Tracked, TxChangeSink, TxChangeSource,
};

/// EVM host and transaction dispatcher.
#[derive_where(Debug)]
pub struct Evm<T: EvmTypes> {
    #[derive_where(skip)]
    spec_id: T::SpecId,
    #[derive_where(skip)]
    execution_config: ExecutionConfig<T>,
    features: EvmFeatures,
    pub(crate) block: BlockEnv<T>,
    registry: TxRegistry<T, TxOutcome<T>>,
    #[derive_where(skip)]
    pub(crate) state: State,
    #[derive_where(skip)]
    precompiles: Box<dyn PrecompileProvider<T>>,
    #[derive_where(skip)]
    interpreter_pool: InterpreterPool<T>,
    #[derive_where(skip)]
    inspector: Option<Box<dyn Inspector<T>>>,
    #[derive_where(skip)]
    running: bool,
    #[cfg(feature = "async")]
    #[derive_where(skip)]
    async_stack: r#async::FiberStack,
    evm_send: bool,
    db_error_code: Option<DbErrorCode>,
}

impl<T: EvmTypes> Evm<T> {
    /// Creates an EVM for `spec_id` with the provided transaction registry, database, and
    /// precompile provider.
    #[inline]
    pub fn new(
        spec_id: T::SpecId,
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxOutcome<T>>,
        database: impl DynDatabase,
        precompiles: impl PrecompileProvider<T>,
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
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxOutcome<T>>,
        database: impl DynDatabase,
        precompiles: impl PrecompileProvider<T>,
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
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxOutcome<T>>,
        database: Box<dyn DynDatabase>,
        precompiles: Box<dyn PrecompileProvider<T>>,
    ) -> Self {
        assert_eq!(
            spec_id.into(),
            execution_config.base_spec_id(),
            "execution config spec mismatch"
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
            running: false,
            #[cfg(feature = "async")]
            async_stack: r#async::FiberStack::default(),
            evm_send: false,
            db_error_code: None,
        }
    }

    #[inline]
    fn contains_precompile(&self, message: &Message<T>) -> bool {
        !message.disable_precompiles && self.precompiles.contains(&message.code_address)
    }

    #[inline]
    fn execute_precompile(
        &mut self,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Result<PrecompileOutput, PrecompileError> {
        let precompiles = self.precompiles.as_mut() as *mut dyn PrecompileProvider<T>;
        let evm = self as *mut Self;
        // SAFETY: Precompile execution may need access to both the provider and the host EVM.
        // The provider is not moved or replaced during this call, and `execute` is expected to
        // preserve `Evm` invariants while using the host reference.
        unsafe {
            let _guard = self.enter_execution();
            (&mut *precompiles)
                .execute(&mut *evm, message, gas)
                .expect("precompile was checked before execution")
        }
    }

    #[inline]
    fn assert_precompiles_mutable(&self) {
        assert!(!self.running, "precompile provider cannot be modified during EVM execution");
    }

    #[inline]
    fn assert_inspector_mutable(&self) {
        assert!(!self.running, "inspector cannot be modified during EVM execution");
    }

    #[inline]
    const fn enter_execution(&mut self) -> ExecutionGuard {
        let was_running = self.running;
        self.running = true;
        ExecutionGuard { running: &mut self.running, was_running }
    }

    /// Returns the transaction handler registry.
    #[inline]
    pub const fn registry(&self) -> &TxRegistry<T, TxOutcome<T>> {
        &self.registry
    }

    /// Returns the backing database.
    #[inline]
    pub fn database(&self) -> &dyn DynDatabase {
        self.state.initial()
    }

    /// Returns the backing database mutably.
    #[inline]
    pub fn database_mut(&mut self) -> &mut dyn DynDatabase {
        self.state.initial_mut()
    }

    /// Returns the accepted database overlay.
    #[inline]
    pub fn overlay_db(&self) -> &CacheDB<Box<dyn DynDatabase>> {
        self.state.overlay_db()
    }

    /// Returns the accepted database overlay mutably.
    #[inline]
    pub fn overlay_db_mut(&mut self) -> &mut CacheDB<Box<dyn DynDatabase>> {
        self.state.overlay_db_mut()
    }

    /// Returns the latest database error code raised during execution.
    #[inline]
    pub const fn db_error_code(&self) -> Option<DbErrorCode> {
        self.db_error_code
    }

    /// Returns account information visible through the accepted state overlay.
    #[inline]
    pub fn account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.state.account_info(address)
    }

    /// Returns account bytecode visible through the accepted state overlay.
    #[inline]
    pub fn account_code(&mut self, address: &Address) -> DbResult<Bytecode> {
        self.state.get_code(address)
    }

    /// Applies borrowed changes to the accepted state overlay.
    #[inline]
    pub fn commit_source<S: TxChangeSource>(&mut self, source: &S) {
        self.state.commit_source(source);
    }

    /// Replaces the backing database.
    #[inline]
    pub fn set_database(&mut self, database: impl DynDatabase) {
        self.state.set_initial(database);
        self.evm_send = false;
    }

    #[cfg(feature = "async")]
    #[inline]
    fn async_stack(&mut self) -> core::ptr::NonNull<r#async::FiberStack> {
        core::ptr::NonNull::from(&mut self.async_stack)
    }

    #[cfg(feature = "async")]
    #[inline]
    fn assert_erased_send(&self) {
        assert!(
            self.evm_send,
            "async EVM execution requires EVM erased fields to be verified as Send with \
             Evm::evm_is_send"
        );
    }

    /// Marks this EVM as thread-sendable after checking the current erased field types.
    ///
    /// This requires no active inspector. Use [`Self::evm_is_send_with_inspector`] when an
    /// inspector is installed.
    #[inline]
    pub fn evm_is_send<D, P>(&mut self) -> &mut Self
    where
        D: DynDatabase + Send,
        P: PrecompileProvider<T> + Send,
    {
        self.assert_database_type::<D>();
        self.assert_precompiles_type::<P>();
        assert!(self.inspector.is_none(), "inspector type mismatch");
        self.evm_send = true;
        self
    }

    /// Marks this EVM as thread-sendable after checking the current erased field types.
    #[inline]
    pub fn evm_is_send_with_inspector<D, P, I>(&mut self) -> &mut Self
    where
        D: DynDatabase + Send,
        P: PrecompileProvider<T> + Send,
        I: Inspector<T> + Send,
    {
        self.assert_database_type::<D>();
        self.assert_precompiles_type::<P>();
        self.assert_inspector_type::<I>();
        self.evm_send = true;
        self
    }

    #[inline]
    fn assert_database_type<D: DynDatabase>(&self) {
        assert_eq!(self.database().type_id(), TypeId::of::<D>(), "database type mismatch");
    }

    #[inline]
    fn assert_precompiles_type<P: PrecompileProvider<T>>(&self) {
        assert_eq!(
            self.precompiles().type_id(),
            TypeId::of::<P>(),
            "precompile provider type mismatch"
        );
    }

    #[inline]
    fn assert_inspector_type<I: Inspector<T>>(&self) {
        let Some(inspector) = self.inspector() else {
            panic!("inspector type mismatch");
        };
        assert_eq!(inspector.type_id(), TypeId::of::<I>(), "inspector type mismatch");
    }

    /// Returns the backing database as `D` if it has that concrete type.
    #[inline]
    pub fn database_as<D: DynDatabase>(&self) -> Option<&D> {
        self.database().downcast_ref()
    }

    /// Returns the backing database mutably as `D` if it has that concrete type.
    #[inline]
    pub fn database_as_mut<D: DynDatabase>(&mut self) -> Option<&mut D> {
        self.database_mut().downcast_mut()
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
    pub fn precompiles(&self) -> &dyn PrecompileProvider<T> {
        self.precompiles.as_ref()
    }

    /// Returns the precompile provider mutably.
    #[inline]
    pub fn precompiles_mut(&mut self) -> &mut dyn PrecompileProvider<T> {
        self.assert_precompiles_mutable();
        self.precompiles.as_mut()
    }

    /// Replaces the precompile provider.
    #[inline]
    pub fn set_precompiles(&mut self, precompiles: impl PrecompileProvider<T>) {
        self.assert_precompiles_mutable();
        self.precompiles = Box::new(precompiles);
        self.evm_send = false;
    }

    /// Returns the precompile provider as `P` if it has that concrete type.
    #[inline]
    pub fn precompiles_as<P: PrecompileProvider<T>>(&self) -> Option<&P> {
        <dyn core::any::Any>::downcast_ref(self.precompiles())
    }

    /// Returns the precompile provider mutably as `P` if it has that concrete type.
    #[inline]
    pub fn precompiles_as_mut<P: PrecompileProvider<T>>(&mut self) -> Option<&mut P> {
        self.assert_precompiles_mutable();
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
        self.assert_inspector_mutable();
        self.inspector.as_deref_mut()
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

    #[inline(never)]
    fn log_eip7708_transfer(&mut self, from: &Address, to: &Address, value: &Word) {
        if self.feature(EvmFeatures::EIP7708)
            && let Some(log) = eip7708_transfer_log(from, to, value)
        {
            self.emit_log(log);
        }
    }

    /// Sets the active execution inspector.
    #[inline]
    pub fn set_inspector<I: Inspector<T> + 'static>(&mut self, inspector: I) {
        self.assert_inspector_mutable();
        self.inspector = Some(Box::new(inspector));
        self.evm_send = false;
    }

    /// Sets the active boxed execution inspector.
    #[inline]
    pub fn set_boxed_inspector(&mut self, inspector: Box<dyn Inspector<T>>) {
        self.assert_inspector_mutable();
        self.inspector = Some(inspector);
        self.evm_send = false;
    }

    /// Removes the active execution inspector.
    #[inline]
    pub fn clear_inspector(&mut self) -> Option<Box<dyn Inspector<T>>> {
        self.assert_inspector_mutable();
        self.evm_send = false;
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
    pub fn spec_id(&self) -> SpecId {
        self.spec_id.into()
    }

    /// Returns the selector-specific runtime specification ID.
    #[inline]
    pub const fn config_spec_id(&self) -> T::SpecId {
        self.spec_id
    }
}

struct ExecutionGuard {
    running: *mut bool,
    was_running: bool,
}

impl Drop for ExecutionGuard {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: The guard is created from an `Evm` field and dropped before that `Evm` can be
        // dropped. It only restores the execution-state flag updated by this guard.
        unsafe {
            *self.running = self.was_running;
        }
    }
}

#[cfg(feature = "async")]
struct SendEvmRef<'a, T: EvmTypes> {
    evm: &'a mut Evm<T>,
}

#[cfg(feature = "async")]
// SAFETY: `SendEvmRef` is only constructed by async entrypoints after `Evm::evm_is_send` has
// verified the concrete erased field types as `Send`.
unsafe impl<T> Send for SendEvmRef<'_, T>
where
    T: EvmTypes,
    T::SpecId: Send,
    T::Tx: Send,
    T::MessageExt: Send,
    T::MessageResultExt: Send,
    T::TxEnvExt: Send,
    T::TxResultExt: Send,
    T::BlockEnvExt: Send,
{
}

#[cfg(feature = "async")]
impl<'a, T: EvmTypes> SendEvmRef<'a, T> {
    #[inline]
    const fn new(evm: &'a mut Evm<T>) -> Self {
        Self { evm }
    }
}

#[cfg(feature = "async")]
impl<T: EvmTypes<Tx: Typed2718, Host = Evm<T>>> SendEvmRef<'_, T> {
    #[inline]
    fn transact(&mut self, tx: &T::Tx) -> HandlerResult<TxOutcome<T>> {
        self.evm.transact(tx).map(PendingTx::commit)
    }
}

impl<T: EvmTypes<Tx: Typed2718, Host = Self>> Evm<T> {
    /// Dispatches the transaction to its handler and returns a pending transaction.
    ///
    /// The returned [`PendingTx`] keeps post-finalization writes in the transaction scratch layer.
    /// Callers must resolve it with [`PendingTx::commit`], [`PendingTx::commit_to`],
    /// [`PendingTx::commit_with`], [`PendingTx::discard`], or [`PendingTx::detach`] before another
    /// transaction can be executed. Dropping the pending
    /// handle is equivalent to [`PendingTx::discard`].
    pub fn transact(&mut self, tx: &T::Tx) -> HandlerResult<PendingTx<'_, T>> {
        self.db_error_code = None;
        let handler = self.registry.try_get_by_type(tx.ty())?;
        let mut result = handler.call(tx, self);
        let mut has_pending_state = false;
        if let Ok(result) = &mut result {
            if let Err(stop) = self.finalize_transaction() {
                result.status = false;
                result.stop = stop;
                result.output = Bytes::new();
                result.logs.clear();
                self.state.clear_transaction_state();
            } else {
                has_pending_state = true;
                result.logs = self.state.take_logs();
            }
            result.db_error_code = self.db_error_code;
        };
        match result {
            Ok(result) => Ok(PendingTx::from_outcome(self, result, has_pending_state)),
            Err(err) => {
                self.state.clear_transaction_state();
                Err(err)
            }
        }
    }

    /// Executes a transaction for its outcome and discards its state changes.
    ///
    /// This is the cheapest convenience entrypoint for `eth_call`-style simulations: execution
    /// output and logs are returned, but pending writes are not accepted and no owned
    /// [`StateChanges`] is materialized.
    pub fn call_tx(&mut self, tx: &T::Tx) -> HandlerResult<TxOutcome<T>> {
        self.transact(tx).map(PendingTx::discard)
    }

    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte on an async
    /// fiber.
    ///
    /// This must be used with an async database adapter such as
    /// [`evm::async::AsyncDb`](crate::evm::async::AsyncDb) to take
    /// advantage of yielding database I/O. With a synchronous database this is mostly equivalent to
    /// running the synchronous transaction on a fiber.
    ///
    /// This commits the pending transaction on the fiber and returns the result-only
    /// [`TxOutcome`].
    ///
    /// This returns a `Send` future. Before calling it, the current erased database, precompile
    /// provider, and optional inspector must be verified with [`Self::evm_is_send`] or
    /// [`Self::evm_is_send_with_inspector`].
    #[cfg(feature = "async")]
    pub fn transact_async<'a>(
        &'a mut self,
        tx: &'a T::Tx,
    ) -> impl Future<Output = r#async::AsyncResult<TxOutcome<T>, registry::HandlerError>> + Send + 'a
    where
        T::Tx: Sync,
        T::TxResultExt: Send,
    {
        self.assert_erased_send();
        let stack = self.async_stack();
        let mut evm = SendEvmRef::new(self);
        // SAFETY: The returned future owns the exclusive `&mut self` borrow, so nothing else can
        // access the EVM stack slot until that future is dropped. The send marker checked above
        // requires all erased EVM fields to have been verified by `Evm::evm_is_send`.
        unsafe { r#async::on_fiber_result_with_stack(stack, move || evm.transact(tx)) }
    }

    /// Dispatches each transaction to its registered EIP-2718 handler and commits it.
    ///
    /// Use [`Self::transact`] directly when the caller wants to choose between commit, discard,
    /// detach, and accumulator/sink commits for each transaction.
    pub fn transact_iter<'a, I>(
        &'a mut self,
        txs: I,
    ) -> impl Iterator<Item = HandlerResult<TxOutcome<T>>> + 'a
    where
        I: IntoIterator<Item = &'a T::Tx>,
        I::IntoIter: 'a,
        T::Tx: 'a,
        Self: 'a,
    {
        txs.into_iter().map(move |tx| self.transact(tx).map(PendingTx::commit))
    }
}

impl<T: EvmTypes<Host = Self>> Evm<T> {
    #[inline(never)]
    fn execute_create_message(
        &mut self,
        tx_env: &TxEnv<T>,
        bytecode: Bytecode,
        message: &mut Message<T>,
        caller_is_static: bool,
    ) -> MessageResult<T> {
        if let Err(stop) = self.prepare_create_message(&bytecode, message) {
            return Self::error_message_result(stop, message.gas_limit);
        }
        let checkpoint = self.state.checkpoint();
        if let Err(stop) = self.create_message_account(message) {
            self.state.rollback(checkpoint, self.features);
            return Self::error_message_result(stop, message.gas_limit);
        }
        message.code_address = message.destination;
        message.disable_precompiles = false;
        let input = core::mem::take(&mut message.input);

        let stop = self.run_interpreter(bytecode, tx_env, message, caller_is_static);
        message.input = input;

        self.finish_create_message_run(checkpoint, &message.destination, message.gas_limit, stop)
    }

    #[inline(never)]
    fn prepare_create_message(
        &mut self,
        bytecode: &Bytecode,
        message: &mut Message<T>,
    ) -> Result<(), InstrStop> {
        let mut address = Address::ZERO;
        self.create_address(&mut address, bytecode, message)?;
        message.destination = address;
        let address = &message.destination;

        let _ = self.state.warm_account(address);

        if message.depth > 0
            && let Err(code) = self.state.increment_nonce(&message.caller)
        {
            return Err(self.db_error_stop(code));
        }

        Ok(())
    }

    #[inline(never)]
    fn create_message_account(&mut self, message: &Message<T>) -> Result<(), InstrStop> {
        let create_result = match self.state.create_account(
            &message.caller,
            &message.destination,
            &message.value,
            self.features,
        ) {
            Ok(result) => result,
            Err(code) => {
                return Err(self.db_error_stop(code));
            }
        };
        create_result?;

        self.log_eip7708_transfer(&message.caller, &message.destination, &message.value);
        Ok(())
    }

    #[inline(never)]
    fn finish_create_message_run(
        &mut self,
        checkpoint: StateCheckpoint,
        address: &Address,
        gas_limit: u64,
        stop: InstrStop,
    ) -> MessageResult<T> {
        let interp = self.interpreter_pool.last_mut().unwrap();
        let mut gas = interp.gas();
        let mut output = Bytes::copy_from_slice(interp.output());
        if stop.is_success() {
            if let Err(stop) = self.validate_create_output(&mut gas, &mut output) {
                self.state.rollback(checkpoint, self.features);
                return MessageResult {
                    stop,
                    gas: Self::message_gas(*gas.tracker(), stop),
                    output,
                    created_address: None,
                    ext: T::MessageResultExt::default(),
                    _non_exhaustive: (),
                };
            }

            if let Err(code) = self.state.set_code(address, Bytecode::new_legacy(output.clone())) {
                self.state.rollback(checkpoint, self.features);
                return Self::error_message_result(self.db_error_stop(code), gas_limit);
            }
        } else {
            self.state.rollback(checkpoint, self.features);
        }

        MessageResult {
            stop,
            gas: Self::message_gas(*gas.tracker(), stop),
            output,
            created_address: stop.is_success().then_some(*address),
            ext: T::MessageResultExt::default(),
            _non_exhaustive: (),
        }
    }

    fn validate_create_output(&self, gas: &mut Gas, output: &mut Bytes) -> Result<(), InstrStop> {
        if self.feature(EvmFeatures::CODE_SIZE_CHECK) && output.len() > self.version().max_code_size
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
        address: &mut Address,
        bytecode: &Bytecode,
        message: &Message<T>,
    ) -> Result<(), InstrStop> {
        let info = if message.value > 0 || message.depth > 0 {
            self.state.account_info(&message.caller).map_err(|code| self.db_error_stop(code))?
        } else {
            None
        };

        if message.value > 0 && info.as_ref().is_none_or(|info| info.balance < message.value) {
            return Err(InstrStop::OutOfFunds);
        }

        // EIP-2681 caps account nonces at u64::MAX; CREATE/CREATE2 return zero instead of
        // wrapping or saturating the creator nonce.
        if message.depth > 0 && info.as_ref().is_some_and(|info| info.nonce == u64::MAX) {
            return Err(InstrStop::Return);
        }

        *address = match message.kind {
            MessageKind::Create if message.depth == 0 => message.destination,
            MessageKind::Create => {
                message.caller.create(info.as_ref().map_or(0, |info| info.nonce))
            }
            MessageKind::Create2 => message.caller.create2(message.salt, bytecode.hash_slow()),
            _ => unreachable!("invalid create message kind"),
        };
        Ok(())
    }

    #[inline(never)]
    fn execute_call_message(
        &mut self,
        tx_env: &TxEnv<T>,
        bytecode: Bytecode,
        message: &mut Message<T>,
        caller_is_static: bool,
    ) -> MessageResult<T> {
        let checkpoint = self.state.checkpoint();
        // EIP-161 state clearing depends on zero-value direct call targets being touched.
        let transfers_balance = matches!(
            message.kind,
            MessageKind::Call | MessageKind::CallCode | MessageKind::StaticCall
        );
        let transfer_succeeded = !transfers_balance
            || match self.state.transfer(&message.caller, &message.destination, &message.value) {
                Ok(result) => result,
                Err(code) => {
                    return Self::error_message_result(self.db_error_stop(code), message.gas_limit);
                }
            };
        if transfers_balance && !transfer_succeeded {
            return Self::error_message_result(InstrStop::OutOfFunds, message.gas_limit);
        }
        if transfers_balance {
            self.log_eip7708_transfer(&message.caller, &message.destination, &message.value);
        }

        if self.contains_precompile(message) {
            return self.execute_call_precompile(checkpoint, message);
        }

        let stop = self.run_interpreter(bytecode, tx_env, message, caller_is_static);

        self.finish_call_message_run(checkpoint, stop)
    }

    #[inline(never)]
    fn execute_call_precompile(
        &mut self,
        checkpoint: StateCheckpoint,
        message: &Message<T>,
    ) -> MessageResult<T> {
        let mut gas = GasTracker::new(message.gas_limit);
        let result = self.execute_precompile(message, &mut gas);
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
            self.state.rollback(checkpoint, self.features);
        }
        MessageResult {
            stop,
            gas: Self::message_gas(gas, stop),
            output,
            created_address: None,
            ext: T::MessageResultExt::default(),
            _non_exhaustive: (),
        }
    }

    #[inline(never)]
    fn finish_call_message_run(
        &mut self,
        checkpoint: StateCheckpoint,
        stop: InstrStop,
    ) -> MessageResult<T> {
        let interp = self.interpreter_pool.last_mut().unwrap();
        let child_gas = interp.gas();
        let output = Bytes::copy_from_slice(interp.output());
        if !stop.is_success() {
            self.state.rollback(checkpoint, self.features);
        }

        MessageResult {
            stop,
            gas: Self::message_gas(*child_gas.tracker(), stop),
            output,
            created_address: None,
            ext: T::MessageResultExt::default(),
            _non_exhaustive: (),
        }
    }

    #[inline]
    fn error_message_result(stop: InstrStop, gas_remaining: u64) -> MessageResult<T> {
        MessageResult { stop, gas: GasTracker::new(gas_remaining), ..MessageResult::default() }
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
        tx_env: &'frame TxEnv<T>,
        message: &'frame Message<T>,
        caller_is_static: bool,
    ) -> InstrStop {
        let mut interp = self.interpreter_pool.pop();
        let _guard = self.enter_execution();
        let interp_ref = interp.as_mut();
        interp_ref.init(bytecode, tx_env, message, caller_is_static);
        // SAFETY: `execution_config` points to a private field that host execution does not
        // replace or mutate, so the pointee remains valid here.
        let execution_config = unsafe { trustme::decouple_lt(&self.execution_config) };
        self.inspect_initialize_interp(interp_ref);
        let inspector = self.inspector.as_deref_mut().map(|inspector| {
            // SAFETY: The inspector is stored in `self` and remains alive for the duration of the
            // interpreter run.
            unsafe { trustme::decouple_lt_mut(inspector) }
        });
        let stop = if let Some(inspector) = inspector {
            interp_ref.run_inspect(execution_config, self, inspector)
        } else {
            interp_ref.run(execution_config, self)
        };
        self.interpreter_pool.push(interp);
        stop
    }

    fn inspect_initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        if let Some(inspector) = &mut self.inspector {
            inspector.initialize_interp(interp);
        }
    }
}

impl<T: EvmTypes<Host = Self>> Host<T> for Evm<T> {
    fn spec_id(&self) -> SpecId {
        self.spec_id()
    }

    fn block_env(&mut self) -> &BlockEnv<T> {
        &self.block
    }

    fn load_account(
        &mut self,
        address: &Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        let is_cold = if self.feature(EvmFeatures::EIP2929) {
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
                self.state.get_code(address).map_err(|code| self.db_error_stop(code))?
            } else {
                Bytecode::default()
            },
            exists,
            is_empty: info.is_empty(),
            is_cold,
            _non_exhaustive: (),
        })
    }

    fn target_is_empty_for_new_account_gas(
        &mut self,
        address: &Address,
        features: EvmFeatures,
    ) -> Result<bool, InstrStop> {
        self.state
            .target_is_empty_for_new_account_gas(address, features)
            .map_err(|code| self.db_error_stop(code))
    }

    fn block_hash(&mut self, number: &Word) -> Result<Option<B256>, InstrStop> {
        self.state.block_hash(number).map_err(|code| self.db_error_stop(code))
    }

    fn sload(
        &mut self,
        address: &Address,
        key: &Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop> {
        let is_cold = self.feature(EvmFeatures::EIP2929) && self.state.warm_storage(address, key);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        Ok(SLoad {
            value: self.state.storage(address, key).map_err(|code| self.db_error_stop(code))?,
            is_cold,
            _non_exhaustive: (),
        })
    }

    fn sstore(
        &mut self,
        address: &Address,
        key: &Word,
        value: &Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop> {
        let is_cold = self.feature(EvmFeatures::EIP2929) && self.state.warm_storage(address, key);
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let mut result =
            self.state.set_storage(address, key, value).map_err(|code| self.db_error_stop(code))?;
        result.is_cold = is_cold;
        Ok(result)
    }

    fn tload(&mut self, address: &Address, key: &Word) -> Word {
        self.state.transient_storage(address, key)
    }

    fn tstore(&mut self, address: &Address, key: &Word, value: &Word) {
        self.state.set_transient_storage(address, key, value);
    }

    fn log(&mut self, log: Log) {
        self.state.log(log);
    }

    #[inline]
    fn execute_message(
        &mut self,
        tx_env: &TxEnv<T>,
        bytecode: Bytecode,
        message: &mut Message<T>,
        caller_is_static: bool,
    ) -> MessageResult<T> {
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
        contract: &Address,
        target: &Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        let is_cold = if self.feature(EvmFeatures::EIP2929) {
            self.state.warm_account(target)
        } else {
            let _ = self.state.warm_account(target);
            false
        };
        if skip_cold_load && is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let features = self.features;
        let target_is_empty_for_new_account_gas =
            self.target_is_empty_for_new_account_gas(target, features)?;
        let previously_destroyed = self.state.is_selfdestructed(contract);
        let balance = self
            .state
            .account_info(contract)
            .map_err(|code| self.db_error_stop(code))?
            .map_or(Word::ZERO, |info| info.balance);
        let should_destroy =
            !self.feature(EvmFeatures::EIP6780) || self.state.is_created_in_transaction(contract);

        if contract != target {
            let transferred = self
                .state
                .transfer(contract, target, &balance)
                .map_err(|code| self.db_error_stop(code))?;
            if transferred {
                self.log_eip7708_transfer(contract, target, &balance);
            }
        } else if should_destroy && !balance.is_zero() {
            if self.feature(EvmFeatures::EIP7708)
                && let Some(log) = eip7708_burn_log(contract, &balance)
            {
                self.emit_log(log);
            }
            self.state
                .add_balance(contract, &Word::ZERO.wrapping_sub(balance))
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
            _non_exhaustive: (),
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
    /// Account bytecode.
    pub code: Bytecode,
    /// Whether the account exists in state.
    pub exists: bool,
    /// Whether the account is empty.
    pub is_empty: bool,
    /// Whether the account access was cold.
    pub is_cold: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Result of an `SLOAD` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SLoad {
    /// Storage slot value.
    pub value: Word,
    /// Whether the storage slot access was cold.
    pub is_cold: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
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
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Gas accounting values reported by transaction execution.
///
/// `tx_gas_used` is the transaction gas-used value exposed by [`TxOutcome::gas_used`] and
/// [`TxResult::gas_used`]. Additional
/// fields make room for block-level accounting such as regular/state gas splits, refunds, and
/// floor-gas accounting without forcing callers to infer them from a single number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TxGas {
    /// Gas used by the transaction.
    pub tx_gas_used: u64,
    /// Regular gas contribution to block gas accounting.
    pub block_regular_gas_used: u64,
    /// State gas contribution to block gas accounting.
    pub block_state_gas_used: u64,
    /// Total gas spent by the transaction before caller-specific fee accounting.
    pub total_spent: u64,
    /// Gas refunded by execution before final refund caps are applied.
    pub refunded: u64,
    /// Floor gas used for forks that apply transaction floor-gas rules.
    pub floor_gas: u64,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl TxGas {
    /// Creates gas accounting from a single gas-used value.
    #[inline]
    pub const fn from_tx_gas_used(tx_gas_used: u64) -> Self {
        Self {
            tx_gas_used,
            block_regular_gas_used: tx_gas_used,
            block_state_gas_used: 0,
            total_spent: tx_gas_used,
            refunded: 0,
            floor_gas: 0,
            _non_exhaustive: (),
        }
    }
}

/// Transaction execution outcome without an owned state diff.
///
/// This is the result-only half of transaction execution: status, gas, output, stop reason, logs,
/// database error handle, and extension data. Logs live here because they are execution output, not
/// database state. Use [`PendingTx::detach`] only when an owned [`StateChanges`] value is required.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::TxResultExt)]
pub struct TxOutcome<T: EvmTypes = crate::BaseEvmTypes> {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas accounting reported by execution.
    pub gas: TxGas,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
    /// Logs emitted by the transaction.
    pub logs: Vec<Log>,
    /// Database error handle, if execution stopped on a database error.
    pub db_error_code: Option<DbErrorCode>,
    /// EVM type-specific extension data.
    pub ext: T::TxResultExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<T: EvmTypes> TxOutcome<T> {
    /// Returns the transaction gas-used value.
    #[inline]
    pub const fn gas_used(&self) -> u64 {
        self.gas.tx_gas_used
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingState {
    Present,
    Cleared,
}

/// A transaction whose post-finalization state is still pending.
///
/// `PendingTx` borrows the EVM mutably until the caller chooses what to do with the pending
/// transaction scratch:
///
/// - [`Self::commit`] accepts the state into the internal accepted overlay;
/// - [`Self::discard`] drops the state and keeps only the outcome;
/// - [`Self::detach`] materializes an owned [`StateChanges`] value without committing it;
/// - [`Self::commit_to`] accepts the state and records it in a block accumulator;
/// - [`Self::commit_with`] accepts the state and first streams it to an external sink.
///
/// Dropping `PendingTx` without calling one of those methods is equivalent to [`Self::discard`].
#[must_use = "pending transaction state must be committed, discarded, or detached"]
pub struct PendingTx<'evm, T: EvmTypes = crate::BaseEvmTypes> {
    evm: &'evm mut Evm<T>,
    outcome: Option<TxOutcome<T>>,
    state: PendingState,
}

impl<T: EvmTypes> fmt::Debug for PendingTx<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingTx")
            .field("has_pending_state", &self.has_pending_state())
            .finish_non_exhaustive()
    }
}

impl<'evm, T: EvmTypes> PendingTx<'evm, T> {
    #[inline]
    const fn from_outcome(
        evm: &'evm mut Evm<T>,
        outcome: TxOutcome<T>,
        has_pending_state: bool,
    ) -> Self {
        Self {
            evm,
            outcome: Some(outcome),
            state: if has_pending_state { PendingState::Present } else { PendingState::Cleared },
        }
    }

    #[inline]
    fn has_pending_state(&self) -> bool {
        self.state == PendingState::Present
    }

    #[inline]
    fn take_outcome(&mut self) -> TxOutcome<T> {
        match self.outcome.take() {
            Some(outcome) => outcome,
            None => unreachable!("pending transaction outcome was already taken"),
        }
    }

    /// Returns the transaction outcome without resolving the pending state.
    #[inline]
    pub fn outcome(&self) -> &TxOutcome<T> {
        match &self.outcome {
            Some(outcome) => outcome,
            None => unreachable!("pending transaction outcome was already taken"),
        }
    }

    /// Accepts the pending transaction into the internal accepted overlay.
    ///
    /// This makes the transaction's state effects visible to later transactions executed by the
    /// same EVM. It clears transaction scratch and returns the result-only [`TxOutcome`].
    ///
    /// The block accumulator/sink variant will be layered on top of this lifecycle; this initial
    /// API commits to the internal accepted overlay only.
    pub fn commit(mut self) -> TxOutcome<T> {
        if self.has_pending_state() {
            self.evm.state.commit_transaction();
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
        self.take_outcome()
    }

    /// Accepts the pending transaction and records its changes in a block accumulator.
    ///
    /// This streams pending changes into `block_state`, commits them to the accepted overlay, and
    /// returns the result-only [`TxOutcome`]. No owned [`StateChanges`] is materialized.
    pub fn commit_to(mut self, block_state: &mut BlockStateAccumulator) -> TxOutcome<T> {
        if self.has_pending_state() {
            match self.evm.state.visit_transaction_changes(block_state) {
                Ok(()) => {}
                Err(err) => match err {},
            }
            self.evm.state.commit_transaction();
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
        self.take_outcome()
    }

    /// Streams pending changes into `sink`, then accepts the transaction.
    ///
    /// If the sink returns an error, the transaction is not committed and the pending handle is
    /// dropped, which discards the transaction scratch. Use infallible sinks on the block hot path.
    pub fn commit_with<S: TxChangeSink>(mut self, sink: &mut S) -> Result<TxOutcome<T>, S::Error> {
        if self.has_pending_state() {
            self.evm.state.visit_transaction_changes(sink)?;
            self.evm.state.commit_transaction();
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
        Ok(self.take_outcome())
    }

    /// Discards the pending transaction state and returns the outcome.
    ///
    /// Discarding does not mutate the accepted overlay and does not materialize [`StateChanges`].
    /// This is the intended path for result-only execution such as `eth_call`.
    pub fn discard(mut self) -> TxOutcome<T> {
        if self.has_pending_state() {
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
        self.take_outcome()
    }

    /// Detaches the pending transaction into an owned state diff without committing it.
    ///
    /// Detaching materializes [`StateChanges`], clears transaction scratch, and returns a
    /// [`TxResult`] that can be moved or stored. The detached state is not accepted into this EVM's
    /// internal overlay unless the caller commits it separately.
    pub fn detach(mut self) -> TxResult<T> {
        let state_changes = if self.has_pending_state() {
            let changes = self.evm.state.build_state_changes();
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
            changes
        } else {
            StateChanges::default()
        };
        let outcome = self.take_outcome();
        TxResult {
            status: outcome.status,
            gas_used: outcome.gas.tx_gas_used,
            stop: outcome.stop,
            output: outcome.output,
            logs: outcome.logs,
            state_changes,
            db_error_code: outcome.db_error_code,
            ext: outcome.ext,
            _non_exhaustive: (),
        }
    }
}

impl<T: EvmTypes> Drop for PendingTx<'_, T> {
    #[inline]
    fn drop(&mut self) {
        if self.has_pending_state() {
            self.evm.state.clear_transaction_state();
            self.state = PendingState::Cleared;
        }
    }
}

/// Result of executing a transaction with an owned state diff.
///
/// This is the materialized shape produced by [`PendingTx::detach`] and system-call execution. It
/// pairs [`TxOutcome`]-style execution output with an owned [`StateChanges`] value. Prefer
/// resolving [`Evm::transact`] with [`PendingTx::commit`] or [`PendingTx::discard`] when an owned
/// write-set is unnecessary.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::TxResultExt)]
pub struct TxResult<T: EvmTypes = crate::BaseEvmTypes> {
    /// Whether execution succeeded.
    pub status: bool,
    /// Gas used by execution.
    pub gas_used: u64,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
    /// Logs emitted by the transaction.
    pub logs: Vec<Log>,
    /// State transition produced by this transaction.
    pub state_changes: StateChanges,
    /// Database error handle, if execution stopped on a database error.
    pub db_error_code: Option<DbErrorCode>,
    /// EVM type-specific extension data.
    pub ext: T::TxResultExt,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

fn eip7708_transfer_log(from: &Address, to: &Address, value: &Word) -> Option<Log> {
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

fn eip7708_burn_log(address: &Address, value: &Word) -> Option<Log> {
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
        env::TxEnv,
        ethereum::RecoveredTxEnvelope,
        interpreter::{GasTracker, Interpreter, MessageKind, op},
        registry::TxRequest,
    };
    use alloc::{string::ToString, vec, vec::Vec};
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
        req: TxRequest<'_, BaseEvmTypes, Recovered<TxLegacy>>,
    ) -> HandlerResult<TxOutcome> {
        let _ = req.host.spec_id();
        Ok(TxOutcome {
            status: true,
            gas: TxGas::from_tx_gas_used(req.tx.nonce + 1),
            ..TxOutcome::default()
        })
    }

    fn handle_test_tx_version(
        req: TxRequest<'_, BaseEvmTypes, Recovered<TxLegacy>>,
    ) -> HandlerResult<TxOutcome> {
        Ok(TxOutcome {
            status: true,
            gas: TxGas::from_tx_gas_used(req.host.version().tx_gas_limit_cap),
            ..TxOutcome::default()
        })
    }

    const LIFECYCLE_ACCOUNT: Address = Address::with_last_byte(0x7a);
    const LIFECYCLE_STORAGE_KEY: Word = Word::from_limbs([1, 0, 0, 0]);

    fn handle_lifecycle_tx(
        req: TxRequest<'_, BaseEvmTypes, Recovered<TxLegacy>>,
    ) -> HandlerResult<TxOutcome> {
        let value = Word::from(req.tx.nonce);
        req.host
            .state
            .set_storage(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY, &value)
            .map_err(registry::HandlerError::Database)?;
        req.host.state.log(Log {
            address: LIFECYCLE_ACCOUNT,
            data: LogData::new_unchecked(vec![], Bytes::new()),
        });
        Ok(TxOutcome {
            status: true,
            gas: TxGas::from_tx_gas_used(req.tx.nonce),
            ..TxOutcome::default()
        })
    }

    fn lifecycle_evm() -> Evm<BaseEvmTypes> {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            RecoveredTxEnvelope::as_legacy,
            handle_lifecycle_tx,
        );
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &LIFECYCLE_ACCOUNT,
            AccountInfo::default().with_balance(Word::from(1)),
        );
        database.insert_account_storage(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY, &Word::from(1));
        Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            registry,
            database,
            Precompiles::base(SpecId::OSAKA),
        )
    }

    #[derive(Clone, Copy)]
    enum PrecompileAccess {
        Mut,
        AsMut,
        Set,
    }

    struct AccessingPrecompile {
        access: PrecompileAccess,
    }

    impl AccessingPrecompile {
        const ADDRESS: Address = Address::with_last_byte(0x42);
    }

    impl PrecompileProvider<BaseEvmTypes> for AccessingPrecompile {
        fn contains(&self, address: &Address) -> bool {
            *address == Self::ADDRESS
        }

        fn execute(
            &mut self,
            evm: &mut Evm<BaseEvmTypes>,
            message: &Message,
            _gas: &mut GasTracker,
        ) -> Option<Result<PrecompileOutput, PrecompileError>> {
            if message.code_address != Self::ADDRESS {
                return None;
            }
            match self.access {
                PrecompileAccess::Mut => {
                    let _ = evm.precompiles_mut();
                }
                PrecompileAccess::AsMut => {
                    let _ = evm.precompiles_as_mut::<Self>();
                }
                PrecompileAccess::Set => evm.set_precompiles(Precompiles::base(SpecId::OSAKA)),
            }
            Some(Ok(PrecompileOutput::new(Bytes::new())))
        }
    }

    fn run_precompile_access(access: PrecompileAccess) {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            AccessingPrecompile { access },
        );
        let message = Message {
            kind: MessageKind::Call,
            depth: 0,
            gas_limit: 30_000,
            destination: AccessingPrecompile::ADDRESS,
            caller: Address::ZERO,
            input: Bytes::new(),
            value: U256::ZERO,
            code_address: AccessingPrecompile::ADDRESS,
            disable_precompiles: false,
            salt: B256::ZERO,
            ext: (),
            _non_exhaustive: (),
        };
        let _ = evm.execute_precompile(&message, &mut GasTracker::new(30_000));
    }

    #[test]
    fn immutable_precompile_access_is_allowed_during_execution() {
        struct ReadingPrecompile;

        impl PrecompileProvider<BaseEvmTypes> for ReadingPrecompile {
            fn contains(&self, address: &Address) -> bool {
                *address == AccessingPrecompile::ADDRESS
            }

            fn execute(
                &mut self,
                evm: &mut Evm<BaseEvmTypes>,
                message: &Message,
                _gas: &mut GasTracker,
            ) -> Option<Result<PrecompileOutput, PrecompileError>> {
                if !self.contains(&message.code_address) {
                    return None;
                }
                let _ = evm.precompiles();
                let _ = evm.precompiles_as::<Self>();
                Some(Ok(PrecompileOutput::new(Bytes::new())))
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            ReadingPrecompile,
        );
        let message = Message {
            kind: MessageKind::Call,
            depth: 0,
            gas_limit: 30_000,
            destination: AccessingPrecompile::ADDRESS,
            caller: Address::ZERO,
            input: Bytes::new(),
            value: U256::ZERO,
            code_address: AccessingPrecompile::ADDRESS,
            disable_precompiles: false,
            salt: B256::ZERO,
            ext: (),
            _non_exhaustive: (),
        };

        evm.execute_precompile(&message, &mut GasTracker::new(30_000)).unwrap();
    }

    #[test]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn precompiles_mut_panics_during_execution() {
        run_precompile_access(PrecompileAccess::Mut);
    }

    #[test]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn precompiles_as_mut_panics_during_execution() {
        run_precompile_access(PrecompileAccess::AsMut);
    }

    #[test]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn set_precompiles_panics_during_execution() {
        run_precompile_access(PrecompileAccess::Set);
    }

    #[derive(Clone, Copy)]
    enum InspectorAccess {
        Mut,
        Set,
        SetBoxed,
        Clear,
    }

    struct AccessingInspector {
        access: InspectorAccess,
        evm: *mut Evm<BaseEvmTypes>,
    }

    unsafe impl Send for AccessingInspector {}

    impl Inspector<BaseEvmTypes> for AccessingInspector {
        fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, BaseEvmTypes>) {
            let evm = unsafe { &mut *self.evm };
            match self.access {
                InspectorAccess::Mut => {
                    let _ = evm.inspector_mut();
                }
                InspectorAccess::Set => evm.set_inspector(NoopInspector),
                InspectorAccess::SetBoxed => evm.set_boxed_inspector(Box::new(NoopInspector)),
                InspectorAccess::Clear => {
                    let _ = evm.clear_inspector();
                }
            }
        }
    }

    struct NoopInspector;

    impl Inspector<BaseEvmTypes> for NoopInspector {}

    fn run_inspector_access(access: InspectorAccess) {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let evm_ptr = &mut evm as *mut Evm<BaseEvmTypes>;
        evm.set_inspector(AccessingInspector { access, evm: evm_ptr });
        let message = Message::default();
        let tx_env = TxEnv::default();
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let _ = evm.run_interpreter(bytecode, &tx_env, &message, false);
    }

    #[test]
    fn immutable_inspector_access_is_allowed_during_execution() {
        struct ReadingInspector {
            evm: *const Evm<BaseEvmTypes>,
        }

        unsafe impl Send for ReadingInspector {}

        impl Inspector<BaseEvmTypes> for ReadingInspector {
            fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, BaseEvmTypes>) {
                let evm = unsafe { &*self.evm };
                let _ = evm.inspector();
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let evm_ptr = &evm as *const Evm<BaseEvmTypes>;
        evm.set_inspector(ReadingInspector { evm: evm_ptr });
        let message = Message::default();
        let tx_env = TxEnv::default();
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));

        let _ = evm.run_interpreter(bytecode, &tx_env, &message, false);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn inspector_mut_panics_during_execution() {
        run_inspector_access(InspectorAccess::Mut);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn set_inspector_panics_during_execution() {
        run_inspector_access(InspectorAccess::Set);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn set_boxed_inspector_panics_during_execution() {
        run_inspector_access(InspectorAccess::SetBoxed);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn clear_inspector_panics_during_execution() {
        run_inspector_access(InspectorAccess::Clear);
    }

    #[test]
    fn passes_evm_to_precompile_provider() {
        #[derive(Default)]
        struct HostObservingPrecompile {
            seen_block_number: Option<U256>,
            seen_message: Option<Message>,
        }

        impl PrecompileProvider<BaseEvmTypes> for HostObservingPrecompile {
            fn contains(&self, address: &Address) -> bool {
                *address == Address::with_last_byte(0x42)
            }

            fn execute(
                &mut self,
                evm: &mut Evm<BaseEvmTypes>,
                message: &Message,
                _gas: &mut GasTracker,
            ) -> Option<Result<PrecompileOutput, PrecompileError>> {
                if !self.contains(&message.code_address) {
                    return None;
                }
                self.seen_block_number = Some(evm.block.number);
                self.seen_message = Some(message.clone());
                Some(Ok(PrecompileOutput::new(Bytes::copy_from_slice(&[0x42]))))
            }
        }

        let address = Address::with_last_byte(0x42);
        let block = BlockEnv { number: U256::from(17), ..BlockEnv::default() };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            block,
            TxRegistry::new(),
            InMemoryDB::default(),
            HostObservingPrecompile::default(),
        );
        let message = Message {
            kind: MessageKind::Call,
            depth: 67,
            gas_limit: 30_000,
            destination: address,
            caller: Address::with_last_byte(0x7a),
            input: Bytes::from_static(b"message input"),
            value: U256::from(99),
            code_address: address,
            disable_precompiles: false,
            salt: B256::ZERO,
            ext: (),
            _non_exhaustive: (),
        };
        let output = evm
            .execute_precompile(&message, &mut GasTracker::new(30_000))
            .expect("precompile succeeds");

        assert_eq!(output.bytes(), &[0x42]);
        assert_eq!(
            evm.precompiles_as::<HostObservingPrecompile>().unwrap().seen_block_number,
            Some(U256::from(17))
        );
        assert_eq!(
            evm.precompiles_as::<HostObservingPrecompile>().unwrap().seen_message,
            Some(message)
        );
    }

    #[test]
    fn precompile_can_call_another_precompile() {
        #[derive(Default)]
        struct NestedPrecompile {
            outer_called: bool,
            inner_called: bool,
        }

        impl NestedPrecompile {
            const OUTER: Address = Address::with_last_byte(0x42);
            const INNER: Address = Address::with_last_byte(0x43);
        }

        impl PrecompileProvider<BaseEvmTypes> for NestedPrecompile {
            fn contains(&self, address: &Address) -> bool {
                *address == Self::OUTER || *address == Self::INNER
            }

            fn execute(
                &mut self,
                evm: &mut Evm<BaseEvmTypes>,
                message: &Message,
                gas: &mut GasTracker,
            ) -> Option<Result<PrecompileOutput, PrecompileError>> {
                if message.code_address == Self::INNER {
                    self.inner_called = true;
                    return Some(Ok(PrecompileOutput::new(Bytes::from_static(b"inner"))));
                }
                if message.code_address != Self::OUTER {
                    return None;
                }
                self.outer_called = true;
                let message = Message {
                    kind: MessageKind::Call,
                    depth: 0,
                    gas_limit: 30_000,
                    destination: Self::INNER,
                    caller: Address::ZERO,
                    input: Bytes::new(),
                    value: U256::ZERO,
                    code_address: Self::INNER,
                    disable_precompiles: false,
                    salt: B256::ZERO,
                    ext: (),
                    _non_exhaustive: (),
                };
                Some(evm.execute_precompile(&message, gas))
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            NestedPrecompile::default(),
        );
        let message = Message {
            kind: MessageKind::Call,
            depth: 0,
            gas_limit: 30_000,
            destination: NestedPrecompile::OUTER,
            caller: Address::ZERO,
            input: Bytes::new(),
            value: U256::ZERO,
            code_address: NestedPrecompile::OUTER,
            disable_precompiles: false,
            salt: B256::ZERO,
            ext: (),
            _non_exhaustive: (),
        };

        let output = evm
            .execute_precompile(&message, &mut GasTracker::new(30_000))
            .expect("precompile succeeds");

        let precompile = evm.precompiles_as::<NestedPrecompile>().unwrap();
        assert_eq!(output.bytes(), b"inner");
        assert!(precompile.outer_called);
        assert!(precompile.inner_called);
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

        assert_eq!(evm.transact(&tx).map(|pending| pending.discard().gas_used()), Ok(42));
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

        assert_eq!(evm.transact(&tx).map(|pending| pending.discard().gas_used()), Ok(42));
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

        assert_eq!(evm.transact(&tx).map(|pending| pending.discard().gas_used()), Ok(42));
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
            .map(|result| result.map(|result| result.gas_used()))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn pending_transaction_discard_drops_state_but_keeps_outcome_logs() {
        let mut evm = lifecycle_evm();
        let outcome =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").discard();

        assert_eq!(outcome.gas_used(), 7);
        assert_eq!(outcome.logs.len(), 1);
        assert_eq!(
            evm.state.storage_ref(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY),
            Some(Word::from(1))
        );
    }

    #[test]
    fn pending_transaction_detach_materializes_without_committing() {
        let mut evm = lifecycle_evm();
        let result =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").detach();

        assert_eq!(result.logs.len(), 1);
        let storage = result
            .state_changes
            .storage
            .get(&LIFECYCLE_ACCOUNT)
            .expect("storage change should be present");
        let slot =
            storage.slots.get(&LIFECYCLE_STORAGE_KEY).expect("storage slot should be present");
        assert_eq!(slot.original, Word::from(1));
        assert_eq!(slot.current, Word::from(7));
        assert_eq!(
            evm.state.storage_ref(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY),
            Some(Word::from(1))
        );
    }

    #[test]
    fn pending_transaction_commit_updates_accepted_overlay() {
        let mut evm = lifecycle_evm();
        let outcome =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").commit();

        assert_eq!(outcome.logs.len(), 1);
        assert_eq!(
            evm.state.storage_ref(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY),
            Some(Word::from(7))
        );

        evm.transact(&test_tx(9)).expect("lifecycle transaction should execute").commit();
        assert_eq!(
            evm.state.storage_ref(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY),
            Some(Word::from(9))
        );
    }

    #[test]
    fn pending_transaction_commit_to_accumulates_block_state() {
        let mut evm = lifecycle_evm();
        let mut block_state = BlockStateAccumulator::new();

        evm.transact(&test_tx(7))
            .expect("lifecycle transaction should execute")
            .commit_to(&mut block_state);
        evm.transact(&test_tx(9))
            .expect("lifecycle transaction should execute")
            .commit_to(&mut block_state);

        let frozen = block_state.freeze();
        let storage = frozen.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].original, Word::from(1));
        assert_eq!(storage[0].current, Word::from(9));
        assert_eq!(
            evm.state.storage_ref(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY),
            Some(Word::from(9))
        );
    }

    #[test]
    fn pending_transaction_commit_with_tee_fans_out_changes() {
        let mut evm = lifecycle_evm();
        let mut left = BlockStateAccumulator::new();
        let mut right = BlockStateAccumulator::new();
        let mut tee = Tee::new(&mut left, &mut right);

        evm.transact(&test_tx(7))
            .expect("lifecycle transaction should execute")
            .commit_with(&mut tee)
            .expect("block accumulators are infallible");

        assert_eq!(left.freeze().storage_sorted()[0].current, Word::from(7));
        assert_eq!(right.freeze().storage_sorted()[0].current, Word::from(7));
    }

    #[test]
    fn dropped_pending_transaction_discards_state() {
        let mut evm = lifecycle_evm();
        drop(evm.transact(&test_tx(7)).expect("lifecycle transaction should execute"));

        assert_eq!(
            evm.state.storage_ref(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY),
            Some(Word::from(1))
        );
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
        let mut message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result =
            Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &mut message, false);
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
        type Error = FailingDbError;

        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(Some(AccountInfo::default()))
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            Err(FailingDbError)
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<Option<B256>, Self::Error> {
            Ok(None)
        }
    }

    #[test]
    fn host_records_database_error_code() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            Db::new(FailingStorageDb),
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::from([0x11; 20]);
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH0, op::SLOAD]));
        let mut message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            ..Message::default()
        };

        let result =
            Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &mut message, false);

        assert_eq!(result.stop, InstrStop::FatalExternalError);
        let error_code = evm.db_error_code().unwrap();
        assert_eq!(evm.database_mut().error(error_code).to_string(), "storage read failed");
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
        let mut message = Message {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 500,
            ..Message::default()
        };

        let result =
            Host::execute_message(&mut evm, &TxEnv::default(), bytecode, &mut message, false);

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert!(!evm.state.is_storage_warm(&contract, &key));
    }

    #[test]
    fn frontier_code_deposit_oog_creates_empty_contract() {
        let caller = Address::from([0x11; 20]);
        let created = Address::from([0x22; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::from(1)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::FRONTIER,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::FRONTIER),
        );
        let mut message = Message {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50,
            ..Message::default()
        };
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 1, op::PUSH1, 0, op::RETURN]));

        let result = Host::execute_message(&mut evm, &TxEnv::default(), code, &mut message, false);
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
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::from(1)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::HOMESTEAD,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::HOMESTEAD),
        );
        let mut message = Message {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50,
            ..Message::default()
        };
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 1, op::PUSH1, 0, op::RETURN]));

        let result = Host::execute_message(&mut evm, &TxEnv::default(), code, &mut message, false);
        assert_eq!(result.stop, InstrStop::OutOfGas);

        evm.state.finalize_transaction_(Version::base(SpecId::HOMESTEAD));
        let changes = evm.state.build_state_changes();
        assert!(!changes.accounts.contains_key(&created));
    }

    #[test]
    fn staticcall_touches_empty_existing_destination() {
        let target = Address::from([0x11; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&target, AccountInfo::default());
        database.insert_account_storage(&target, &Word::ZERO, &Word::from(1));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let mut message = Message {
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
            &mut message,
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
            .insert_account_info(&destination, AccountInfo::default().with_balance(Word::from(1)));
        database.insert_account_info(&code_address, AccountInfo::default());
        database.insert_account_storage(&code_address, &Word::ZERO, &Word::from(1));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let mut message = Message {
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
            &mut message,
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
        state.add_balance(&from, &U256::from(10)).unwrap();

        assert!(state.transfer(&from, &to, &U256::from(7)).unwrap());
        assert_eq!(
            state.account_info(&from).expect("sender account should exist").unwrap().balance,
            U256::from(3)
        );
        assert_eq!(
            state.account_info(&to).expect("recipient account should exist").unwrap().balance,
            U256::from(7)
        );
    }

    #[test]
    fn amsterdam_call_value_emits_eip7708_log() {
        let caller = Address::from([0x01; 20]);
        let target = Address::from([0x02; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(U256::from(10)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        let mut message = Message {
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
            &mut message,
            false,
        );
        assert!(result.stop.is_success());

        let version = *evm.version();
        evm.state.finalize_transaction_(&version);
        let logs = evm.state.take_logs();
        let _changes = evm.state.build_state_changes();
        assert_eq!(logs.len(), 1);
        let log = &logs[0];
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
