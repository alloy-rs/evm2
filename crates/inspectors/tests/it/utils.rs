use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use colorchoice::ColorChoice;
use evm2::{
    BaseEvmTypes, Evm, Inspector, Precompiles, TxResult, env as evm_env,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::StateChanges,
    interpreter::{Interpreter, Message, MessageResult},
    registry::HandlerError,
};
use evm2_inspectors::tracing::{
    TraceBlockEnv, TraceTxEnv, TraceWriter, TraceWriterConfig, TracingInspector,
    geth::{TraceExecutionResult, TraceStateChanges},
};

pub use evm2::{
    SpecId,
    bytecode::{Bytecode, opcode},
    evm::{AccountInfo, CacheDB, EmptyDB},
};

pub type TransactTo = TxKind;

pub const BLOB_BASE_FEE_UPDATE_FRACTION_CANCUN: u64 = 3_338_477;
pub const ETH_TRANSFER_LOG_ADDRESS: Address = evm2::SYSTEM_ADDRESS;

#[derive(Clone, Copy, Debug, Default)]
pub struct BlobExcessGasAndPrice {
    pub excess_blob_gas: u64,
    pub blob_basefee: u64,
}

impl BlobExcessGasAndPrice {
    pub const fn new(excess_blob_gas: u64, blob_basefee: u64) -> Self {
        Self { excess_blob_gas, blob_basefee }
    }
}

#[derive(Clone, Debug)]
pub struct BlockEnv {
    pub number: U256,
    pub beneficiary: Address,
    pub timestamp: U256,
    pub gas_limit: U256,
    pub basefee: u64,
    pub difficulty: U256,
    pub prevrandao: U256,
    pub blob_basefee: u64,
    pub blob_excess_gas_and_price: Option<BlobExcessGasAndPrice>,
}

impl Default for BlockEnv {
    fn default() -> Self {
        let block = evm_env::BlockEnv::<BaseEvmTypes>::default();
        Self {
            number: block.number,
            beneficiary: block.beneficiary,
            timestamp: block.timestamp,
            gas_limit: block.gas_limit,
            basefee: block.basefee.try_into().unwrap_or(u64::MAX),
            difficulty: block.difficulty,
            prevrandao: block.prevrandao,
            blob_basefee: block.blob_basefee.try_into().unwrap_or(u64::MAX),
            blob_excess_gas_and_price: None,
        }
    }
}

impl From<&BlockEnv> for evm_env::BlockEnv {
    fn from(block: &BlockEnv) -> Self {
        Self {
            number: block.number,
            beneficiary: block.beneficiary,
            timestamp: block.timestamp,
            gas_limit: block.gas_limit,
            basefee: U256::from(block.basefee),
            difficulty: block.difficulty,
            prevrandao: block.prevrandao,
            blob_basefee: U256::from(
                block
                    .blob_excess_gas_and_price
                    .map_or(block.blob_basefee, |blob| blob.blob_basefee),
            ),
            slot_num: U256::ZERO,
            ext: (),
            _non_exhaustive: (),
        }
    }
}

impl TraceBlockEnv for BlockEnv {
    fn trace_block_number(&self) -> u64 {
        self.number.try_into().unwrap_or(u64::MAX)
    }

    fn trace_base_fee(&self) -> u64 {
        self.basefee
    }
}

#[derive(Clone, Debug)]
pub struct TxEnv {
    pub caller: Address,
    pub gas_limit: u64,
    pub gas_price: u128,
    pub gas_priority_fee: Option<u128>,
    pub kind: TransactTo,
    pub data: Bytes,
    pub nonce: u64,
    pub value: U256,
    pub chain_id: Option<u64>,
    pub blob_hashes: Vec<B256>,
    pub max_fee_per_blob_gas: u128,
}

impl Default for TxEnv {
    fn default() -> Self {
        Self {
            caller: Address::ZERO,
            gas_limit: 0,
            gas_price: 0,
            gas_priority_fee: None,
            kind: TransactTo::Call(Address::ZERO),
            data: Bytes::new(),
            nonce: 0,
            value: U256::ZERO,
            chain_id: None,
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: 0,
        }
    }
}

impl TxEnv {
    pub fn builder() -> TxEnvBuilder {
        TxEnvBuilder(Self::default())
    }

    pub fn modify(self) -> TxEnvBuilder {
        TxEnvBuilder(self)
    }
}

impl TraceTxEnv for TxEnv {
    fn trace_gas_limit(&self) -> u64 {
        self.gas_limit
    }

    fn trace_caller(&self) -> Address {
        self.caller
    }
}

pub struct TxEnvBuilder(TxEnv);

impl TxEnvBuilder {
    pub fn caller(mut self, caller: Address) -> Self {
        self.0.caller = caller;
        self
    }

    pub fn gas_limit(mut self, gas_limit: u64) -> Self {
        self.0.gas_limit = gas_limit;
        self
    }

    pub fn gas_price(mut self, gas_price: u128) -> Self {
        self.0.gas_price = gas_price;
        self
    }

    pub fn gas_priority_fee(mut self, gas_priority_fee: Option<u128>) -> Self {
        self.0.gas_priority_fee = gas_priority_fee;
        self
    }

    pub fn kind(mut self, kind: TransactTo) -> Self {
        self.0.kind = kind;
        self
    }

    pub fn data(mut self, data: Bytes) -> Self {
        self.0.data = data;
        self
    }

    pub fn nonce(mut self, nonce: u64) -> Self {
        self.0.nonce = nonce;
        self
    }

    pub fn value(mut self, value: U256) -> Self {
        self.0.value = value;
        self
    }

    pub fn build_fill(self) -> TxEnv {
        let mut tx = self.0;
        if tx.gas_limit == 0 {
            tx.gas_limit = 1_000_000;
        }
        tx
    }
}

#[derive(Clone, Debug)]
pub struct Context {
    pub db: CacheDB<EmptyDB>,
    tx: TxEnv,
    block: BlockEnv,
    spec: SpecId,
}

impl Default for Context {
    fn default() -> Self {
        Self::mainnet()
    }
}

impl Context {
    pub fn mainnet() -> Self {
        Self {
            db: CacheDB::new(EmptyDB::default()),
            tx: TxEnv::default(),
            block: BlockEnv::default(),
            spec: SpecId::OSAKA,
        }
    }

    pub fn with_db(mut self, db: CacheDB<EmptyDB>) -> Self {
        self.db = db;
        self
    }

    pub fn modify_db_chained(mut self, f: impl FnOnce(&mut CacheDB<EmptyDB>)) -> Self {
        f(&mut self.db);
        self
    }

    pub fn modify_cfg_chained(mut self, f: impl FnOnce(&mut CfgEnv)) -> Self {
        let mut cfg = CfgEnv { spec: self.spec };
        f(&mut cfg);
        self.spec = cfg.spec;
        self
    }

    pub fn modify_block_chained(mut self, f: impl FnOnce(&mut BlockEnv)) -> Self {
        f(&mut self.block);
        self
    }

    pub fn modify_tx_chained(mut self, f: impl FnOnce(&mut TxEnv)) -> Self {
        f(&mut self.tx);
        self
    }

    pub fn modify_tx(&mut self, f: impl FnOnce(&mut TxEnv)) {
        f(&mut self.tx);
    }

    pub fn build_mainnet(self) -> TestEvm {
        TestEvm { ctx: self }
    }

    pub fn build_mainnet_with_inspector<I: InspectorSlot>(
        self,
        inspector: I,
    ) -> TestEvmWithInspector<I> {
        self.build_mainnet().with_inspector(inspector)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CfgEnv {
    pub spec: SpecId,
}

#[derive(Debug)]
pub struct TestEvm {
    pub ctx: Context,
}

impl TestEvm {
    pub fn ctx(&mut self) -> &mut Context {
        &mut self.ctx
    }

    pub fn with_inspector<I: InspectorSlot>(self, inspector: I) -> TestEvmWithInspector<I> {
        TestEvmWithInspector { ctx: self.ctx, inspector }
    }

    pub fn inspect_tx(&mut self, tx: TxEnv) -> Result<ResultAndState, HandlerError> {
        self.ctx.inspect_tx::<NoopInspector>(tx, None)
    }

    pub fn inspect_tx_commit(&mut self, tx: TxEnv) -> Result<ExecutionResult, HandlerError> {
        let res = self.inspect_tx(tx)?;
        self.ctx.db.commit(res.state.clone());
        Ok(res.result)
    }
}

#[derive(Debug)]
pub struct TestEvmWithInspector<I> {
    pub ctx: Context,
    pub inspector: I,
}

impl<I: InspectorSlot> TestEvmWithInspector<I> {
    pub fn ctx(&mut self) -> &mut Context {
        &mut self.ctx
    }

    pub fn ctx_inspector(&mut self) -> (&mut Context, &mut I::Target) {
        (&mut self.ctx, self.inspector.inspector_mut())
    }

    pub fn inspector(&mut self) -> &mut I::Target {
        self.inspector.inspector_mut()
    }

    pub fn into_inspector(self) -> I {
        self.inspector
    }

    pub fn inspect_tx(&mut self, tx: TxEnv) -> Result<ResultAndState, HandlerError> {
        let inspector = self.inspector.inspector_mut() as *mut I::Target;
        let res = self.ctx.inspect_tx(tx, Some(inspector))?;
        let inspector = self.inspector.inspector_mut();
        let any = inspector as &mut dyn core::any::Any;
        if let Some(tracer) = any.downcast_mut::<TracingInspector>() {
            tracer.fill_storage_changes(&res.state);
        }
        Ok(res)
    }

    pub fn inspect_tx_commit(&mut self, tx: TxEnv) -> Result<ExecutionResult, HandlerError> {
        let res = self.inspect_tx(tx)?;
        self.ctx.db.commit(res.state.clone());
        Ok(res.result)
    }

    pub fn with_inspector<J: InspectorSlot>(self, inspector: J) -> TestEvmWithInspector<J> {
        TestEvmWithInspector { ctx: self.ctx, inspector }
    }
}

impl TestEvmWithInspector<TracingInspector> {
    pub fn set_inspector(&mut self, inspector: TracingInspector) {
        self.inspector = inspector;
    }

    pub fn inspect(
        &mut self,
        tx: TxEnv,
        inspector: TracingInspector,
    ) -> Result<ResultAndState, HandlerError> {
        self.set_inspector(inspector);
        self.inspect_tx(tx)
    }
}

impl Context {
    pub fn tx(&self) -> &TxEnv {
        &self.tx
    }

    pub fn block(&self) -> &BlockEnv {
        &self.block
    }

    pub fn db(&self) -> &CacheDB<EmptyDB> {
        &self.db
    }

    pub fn db_ref(&self) -> &CacheDB<EmptyDB> {
        &self.db
    }

    pub fn db_mut(&mut self) -> &mut CacheDB<EmptyDB> {
        &mut self.db
    }

    fn inspect_tx<I: Inspector<BaseEvmTypes>>(
        &mut self,
        tx: TxEnv,
        inspector: Option<*mut I>,
    ) -> Result<ResultAndState, HandlerError> {
        let mut evm = Evm::<BaseEvmTypes>::new(
            self.spec,
            (&self.block).into(),
            ethereum_tx_registry(self.spec),
            self.db.clone(),
            Precompiles::base(self.spec),
        );
        if let Some(inspector) = inspector {
            evm.set_inspector(RawInspector { inspector });
        }

        let created = match tx.kind {
            TransactTo::Create => Some(tx.caller.create(tx.nonce)),
            TransactTo::Call(_) => None,
        };
        let envelope = tx.envelope();
        let result = evm.transact(&envelope)?;
        self.tx = tx;
        Ok(ResultAndState::new(result, created))
    }
}

impl TxEnv {
    fn envelope(&self) -> RecoveredTxEnvelope {
        RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy {
                chain_id: self.chain_id,
                nonce: self.nonce,
                gas_price: self.gas_price,
                gas_limit: self.gas_limit,
                to: self.kind,
                value: self.value,
                input: self.data.clone(),
            },
            self.caller,
        ))
    }
}

#[derive(Clone, Debug)]
pub struct ResultAndState {
    pub result: ExecutionResult,
    pub state: StateChanges,
}

impl ResultAndState {
    fn new(result: TxResult, created: Option<Address>) -> Self {
        let state = result.state_changes.clone();
        Self { result: ExecutionResult::from_tx_result(result, created), state }
    }
}

impl TraceStateChanges for ResultAndState {
    fn state_changes(&self) -> &StateChanges {
        &self.state
    }
}

impl TraceExecutionResult for ResultAndState {
    fn trace_gas_used(&self) -> u64 {
        self.result.gas_used()
    }

    fn trace_output(&self) -> Bytes {
        self.result.output().unwrap_or_default().clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionResult {
    Success { output: Output, gas_used: u64 },
    Revert { output: Bytes, gas_used: u64 },
    Halt { reason: evm2::interpreter::InstrStop, gas_used: u64 },
}

impl ExecutionResult {
    fn from_tx_result(result: TxResult, created: Option<Address>) -> Self {
        if result.status {
            let output = if created.is_some() {
                Output::Create(result.output, created)
            } else {
                Output::Call(result.output)
            };
            Self::Success { output, gas_used: result.gas_used }
        } else if result.stop.is_revert() {
            Self::Revert { output: result.output, gas_used: result.gas_used }
        } else {
            Self::Halt { reason: result.stop, gas_used: result.gas_used }
        }
    }

    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    pub const fn tx_gas_used(&self) -> u64 {
        match self {
            Self::Success { gas_used, .. }
            | Self::Revert { gas_used, .. }
            | Self::Halt { gas_used, .. } => *gas_used,
        }
    }

    pub const fn gas_used(&self) -> u64 {
        self.tx_gas_used()
    }

    pub const fn output(&self) -> Option<&Bytes> {
        match self {
            Self::Success { output, .. } => Some(output.data()),
            Self::Revert { output, .. } => Some(output),
            Self::Halt { .. } => None,
        }
    }

    pub const fn created_address(&self) -> Option<Address> {
        match self {
            Self::Success { output: Output::Create(_, address), .. } => *address,
            _ => None,
        }
    }
}

impl TraceExecutionResult for ExecutionResult {
    fn trace_gas_used(&self) -> u64 {
        self.tx_gas_used()
    }

    fn trace_output(&self) -> Bytes {
        self.output().cloned().unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Call(Bytes),
    Create(Bytes, Option<Address>),
}

impl Output {
    pub const fn data(&self) -> &Bytes {
        match self {
            Self::Call(data) | Self::Create(data, _) => data,
        }
    }
}

pub struct DeployResult {
    result: ExecutionResult,
}

impl DeployResult {
    pub const fn created_address(&self) -> Option<Address> {
        self.result.created_address()
    }
}

pub fn write_traces(tracer: &TracingInspector) -> String {
    write_traces_with(tracer, TraceWriterConfig::new().color_choice(ColorChoice::Never))
}

pub fn write_traces_with(tracer: &TracingInspector, config: TraceWriterConfig) -> String {
    let mut w = TraceWriter::with_config(Vec::<u8>::new(), config);
    w.write_arena(tracer.traces()).expect("failed to write traces to Vec<u8>");
    String::from_utf8(w.into_writer()).expect("trace writer wrote invalid UTF-8")
}

pub fn print_traces(tracer: &TracingInspector) {
    println!("{}", write_traces_with(tracer, TraceWriterConfig::new()));
}

pub trait TracingInspectorExt {
    fn with_transaction_gas_limit(self, gas_limit: u64) -> Self;
}

impl TracingInspectorExt for TracingInspector {
    fn with_transaction_gas_limit(mut self, gas_limit: u64) -> Self {
        self.set_transaction_gas_limit(gas_limit);
        self
    }
}

pub fn deploy_contract(
    evm: &mut TestEvm,
    code: Bytes,
    deployer: Address,
    spec: SpecId,
) -> DeployResult {
    evm.ctx.spec = spec;
    let value = evm.ctx.tx.value;
    let result = evm
        .inspect_tx_commit(TxEnv {
            caller: deployer,
            gas_limit: 1_000_000,
            kind: TransactTo::Create,
            data: code,
            value,
            nonce: evm.ctx.tx.nonce,
            ..Default::default()
        })
        .expect("Expect to be executed");
    evm.ctx.tx.nonce += 1;
    DeployResult { result }
}

pub fn inspect_deploy_contract<I: InspectorSlot>(
    evm: &mut TestEvmWithInspector<I>,
    code: Bytes,
    deployer: Address,
    spec: SpecId,
) -> DeployResult {
    evm.ctx.spec = spec;
    let value = evm.ctx.tx.value;
    let result = evm
        .inspect_tx_commit(TxEnv {
            caller: deployer,
            gas_limit: 1_000_000,
            kind: TransactTo::Create,
            data: code,
            value,
            nonce: evm.ctx.tx.nonce,
            ..Default::default()
        })
        .expect("Expect to be executed");
    evm.ctx.tx.nonce += 1;
    DeployResult { result }
}

pub trait DatabaseCommit {
    fn commit(&mut self, changes: StateChanges);
}

impl DatabaseCommit for CacheDB<EmptyDB> {
    fn commit(&mut self, changes: StateChanges) {
        evm2::evm::DatabaseCommit::commit(self, &changes);
    }
}

pub struct LoadedAccountMut<'a> {
    pub info: &'a mut AccountInfo,
}

pub trait TestDbExt {
    fn load_account(&mut self, address: Address) -> Result<LoadedAccountMut<'_>, ()>;
}

impl TestDbExt for CacheDB<EmptyDB> {
    fn load_account(&mut self, address: Address) -> Result<LoadedAccountMut<'_>, ()> {
        let info = self.cache.accounts.entry(address).or_default();
        Ok(LoadedAccountMut { info })
    }
}

pub trait InspectorSlot {
    type Target: Inspector<BaseEvmTypes>;

    fn inspector_mut(&mut self) -> &mut Self::Target;
}

impl<I: Inspector<BaseEvmTypes>> InspectorSlot for &mut I {
    type Target = I;

    fn inspector_mut(&mut self) -> &mut Self::Target {
        self
    }
}

macro_rules! impl_owned_inspector_slot {
    ($($ty:path),* $(,)?) => {
        $(
            impl InspectorSlot for $ty {
                type Target = Self;

                fn inspector_mut(&mut self) -> &mut Self::Target {
                    self
                }
            }
        )*
    };
}

impl_owned_inspector_slot!(
    evm2_inspectors::access_list::AccessListInspector,
    evm2_inspectors::transfer::TransferInspector,
    evm2_inspectors::tracing::DebugInspector,
    evm2_inspectors::tracing::MuxInspector,
    evm2_inspectors::tracing::TracingInspector,
);

#[cfg(any())]
impl_owned_inspector_slot!(evm2_inspectors::tracing::js::JsInspector);

struct RawInspector<I> {
    inspector: *mut I,
}

struct NoopInspector;

impl Inspector<BaseEvmTypes> for NoopInspector {}

impl<I> RawInspector<I> {
    fn inner(&mut self) -> &mut I {
        // SAFETY: `RawInspector` is installed only for the duration of one synchronous
        // transaction, and the caller clears the temporary EVM immediately after execution.
        unsafe { &mut *self.inspector }
    }
}

impl<I: Inspector<BaseEvmTypes>> Inspector<BaseEvmTypes> for RawInspector<I> {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, BaseEvmTypes>) {
        self.inner().initialize_interp(interp);
    }

    fn step(&mut self, interp: &mut Interpreter<'_, BaseEvmTypes>) {
        self.inner().step(interp);
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, BaseEvmTypes>) {
        self.inner().step_end(interp);
    }

    fn log(&mut self, log: &alloy_primitives::Log) {
        self.inner().log(log);
    }

    fn call(&mut self, message: &mut Message<BaseEvmTypes>) -> Option<MessageResult<BaseEvmTypes>> {
        self.inner().call(message)
    }

    fn call_end(
        &mut self,
        message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.inner().call_end(message, result);
    }

    fn create(
        &mut self,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.inner().create(message)
    }

    fn create_end(
        &mut self,
        message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.inner().create_end(message, result);
    }

    fn selfdestruct(&mut self, contract: &Address, target: &Address, value: &U256) {
        self.inner().selfdestruct(contract, target, value);
    }
}
