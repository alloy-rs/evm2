use crate::tracing::{
    FourByteInspector, MuxInspector, TracingInspector, TracingInspectorConfig, TransactionContext,
};
#[cfg(feature = "js-tracer")]
use alloc::boxed::Box;
use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::{
    CallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerType,
    GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace, NoopFrame, PreStateConfig,
    erc7562::Erc7562Config, mux::MuxConfig,
};
use evm2::{
    Evm, EvmTypes, Inspector,
    env::{BlockEnv, TxEnv},
    evm::{CacheDB, EmptyDB, StateChanges},
    interpreter::{Interpreter, Message, MessageResult},
};
use thiserror::Error;

/// Transaction fields needed by debug trace finalization.
pub trait TraceTxEnv {
    /// Returns transaction gas limit.
    fn trace_gas_limit(&self) -> u64;

    /// Returns transaction caller.
    fn trace_caller(&self) -> Address;

    /// Returns transaction target kind.
    fn trace_kind(&self) -> TxKind {
        TxKind::Call(Address::ZERO)
    }

    /// Returns transaction input.
    fn trace_input(&self) -> Bytes {
        Bytes::new()
    }

    /// Returns transaction gas price.
    fn trace_gas_price(&self) -> u128 {
        0
    }

    /// Returns transaction value.
    fn trace_value(&self) -> U256 {
        U256::ZERO
    }
}

impl<T: EvmTypes> TraceTxEnv for TxEnv<T> {
    fn trace_gas_limit(&self) -> u64 {
        0
    }

    fn trace_caller(&self) -> Address {
        self.origin
    }
}

/// Block fields needed by debug trace finalization.
pub trait TraceBlockEnv {
    /// Returns the block number.
    fn trace_block_number(&self) -> u64;

    /// Returns the block beneficiary.
    fn trace_coinbase(&self) -> Address {
        Address::ZERO
    }

    /// Returns the block timestamp.
    fn trace_timestamp(&self) -> U256 {
        U256::ZERO
    }

    /// Returns the block base fee.
    fn trace_base_fee(&self) -> u64;
}

impl<T: EvmTypes> TraceBlockEnv for BlockEnv<T> {
    fn trace_block_number(&self) -> u64 {
        self.number.try_into().unwrap_or(u64::MAX)
    }

    fn trace_coinbase(&self) -> Address {
        self.beneficiary
    }

    fn trace_timestamp(&self) -> U256 {
        self.timestamp
    }

    fn trace_base_fee(&self) -> u64 {
        self.basefee.try_into().unwrap_or(u64::MAX)
    }
}

/// Transaction result fields needed by debug trace finalization.
#[derive(Clone, Copy, Debug)]
pub struct DebugTraceResult<'a> {
    /// Whether execution succeeded.
    pub status: bool,
    /// Transaction gas used.
    pub gas_used: u64,
    /// Interpreter stop reason.
    pub stop: evm2::interpreter::InstrStop,
    /// Transaction output.
    pub return_value: &'a Bytes,
    /// Created contract address for successful create transactions.
    pub created_address: Option<Address>,
    /// Transaction state changes.
    pub state: &'a StateChanges,
    /// Backing cache database for JavaScript result finalization.
    pub db: Option<&'a CacheDB<EmptyDB>>,
}

impl<'a> DebugTraceResult<'a> {
    /// Creates a new debug trace result view.
    pub const fn new(gas_used: u64, return_value: &'a Bytes, state: &'a StateChanges) -> Self {
        Self {
            status: true,
            gas_used,
            stop: evm2::interpreter::InstrStop::Stop,
            return_value,
            created_address: None,
            state,
            db: None,
        }
    }

    /// Sets the execution status and stop reason.
    pub const fn with_status(mut self, status: bool, stop: evm2::interpreter::InstrStop) -> Self {
        self.status = status;
        self.stop = stop;
        self
    }

    /// Sets the created address.
    pub const fn with_created_address(mut self, created_address: Option<Address>) -> Self {
        self.created_address = created_address;
        self
    }

    /// Sets the database exposed to JavaScript result finalization.
    pub const fn with_db(mut self, db: &'a CacheDB<EmptyDB>) -> Self {
        self.db = Some(db);
        self
    }
}

/// Inspector for the `debug` API
///
/// This inspector is used to trace the execution of a transaction or call and supports all variants
/// of [`GethDebugTracerType`].
///
/// This inspector can be re-used for tracing multiple transactions. This is supported by
/// requiring caller to invoke [`DebugInspector::fuse`] after each transaction. See method
/// documentation for more details.
#[derive(Debug)]
pub enum DebugInspector {
    /// FourByte tracer
    FourByte(FourByteInspector),
    /// CallTracer
    CallTracer(TracingInspector, CallConfig),
    /// PreStateTracer
    PreStateTracer(TracingInspector, PreStateConfig),
    /// Noop tracer
    Noop,
    /// Mux tracer
    Mux(MuxInspector, MuxConfig),
    /// FlatCallTracer
    FlatCallTracer(TracingInspector),
    /// Erc7562Tracer
    Erc7562Tracer(TracingInspector, Erc7562Config),
    /// Default tracer
    Default(TracingInspector, GethDefaultTracingOptions),
    /// JS tracer
    #[cfg(feature = "js-tracer")]
    Js(Box<crate::tracing::js::JsInspector>),
}

impl DebugInspector {
    /// Creates a fresh copy of this inspector, resetting all execution state.
    pub fn try_clone(&self) -> Result<Self, DebugInspectorError> {
        Ok(match self {
            Self::FourByte(inspector) => Self::FourByte(inspector.clone()),
            Self::CallTracer(inspector, config) => Self::CallTracer(inspector.clone(), *config),
            Self::PreStateTracer(inspector, config) => {
                Self::PreStateTracer(inspector.clone(), *config)
            }
            Self::Noop => Self::Noop,
            Self::Mux(inspector, config) => Self::Mux(inspector.clone(), config.clone()),
            Self::FlatCallTracer(inspector) => Self::FlatCallTracer(inspector.clone()),
            Self::Erc7562Tracer(inspector, config) => {
                Self::Erc7562Tracer(inspector.clone(), config.clone())
            }
            Self::Default(inspector, config) => Self::Default(inspector.clone(), *config),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => Self::Js(inspector.try_clone()?.into()),
        })
    }

    /// Create a new `DebugInspector` from the given tracing options.
    pub fn new(opts: GethDebugTracingOptions) -> Result<Self, DebugInspectorError> {
        let GethDebugTracingOptions { config, tracer, tracer_config, .. } = opts;

        let this = if let Some(tracer) = tracer {
            #[allow(unreachable_patterns)]
            match tracer {
                GethDebugTracerType::BuiltInTracer(tracer) => match tracer {
                    GethDebugBuiltInTracerType::FourByteTracer => {
                        Self::FourByte(FourByteInspector::default())
                    }
                    GethDebugBuiltInTracerType::CallTracer => {
                        let config = tracer_config
                            .into_call_config()
                            .map_err(|_| DebugInspectorError::InvalidTracerConfig)?;

                        Self::CallTracer(
                            TracingInspector::new(TracingInspectorConfig::from_geth_call_config(
                                &config,
                            )),
                            config,
                        )
                    }
                    GethDebugBuiltInTracerType::PreStateTracer => {
                        let config = tracer_config
                            .into_pre_state_config()
                            .map_err(|_| DebugInspectorError::InvalidTracerConfig)?;

                        Self::PreStateTracer(
                            TracingInspector::new(
                                TracingInspectorConfig::from_geth_prestate_config(&config),
                            ),
                            config,
                        )
                    }
                    GethDebugBuiltInTracerType::NoopTracer => Self::Noop,
                    GethDebugBuiltInTracerType::MuxTracer => {
                        let config = tracer_config
                            .into_mux_config()
                            .map_err(|_| DebugInspectorError::InvalidTracerConfig)?;

                        Self::Mux(MuxInspector::try_from_config(config.clone())?, config)
                    }
                    GethDebugBuiltInTracerType::FlatCallTracer => {
                        let flat_call_config = tracer_config
                            .into_flat_call_config()
                            .map_err(|_| DebugInspectorError::InvalidTracerConfig)?;

                        Self::FlatCallTracer(TracingInspector::new(
                            TracingInspectorConfig::from_flat_call_config(&flat_call_config),
                        ))
                    }
                    GethDebugBuiltInTracerType::Erc7562Tracer => {
                        let config = if tracer_config.is_null() {
                            Erc7562Config::default()
                        } else {
                            tracer_config
                                .from_value()
                                .map_err(|_| DebugInspectorError::InvalidTracerConfig)?
                        };

                        Self::Erc7562Tracer(
                            TracingInspector::new(
                                TracingInspectorConfig::from_geth_erc7562_config(&config),
                            ),
                            config,
                        )
                    }
                    _ => {
                        // Note: this match is non-exhaustive in case we need to add support for
                        // additional tracers
                        return Err(DebugInspectorError::UnsupportedTracer);
                    }
                },
                #[cfg(not(feature = "js-tracer"))]
                GethDebugTracerType::JsTracer(_) => {
                    return Err(DebugInspectorError::JsTracerNotEnabled);
                }
                #[cfg(feature = "js-tracer")]
                GethDebugTracerType::JsTracer(code) => Self::Js(
                    crate::tracing::js::JsInspector::new(code, tracer_config.into_json())?.into(),
                ),
                _ => {
                    // Note: this match is non-exhaustive in case we need to add support for
                    // additional tracers
                    return Err(DebugInspectorError::UnsupportedTracer);
                }
            }
        } else {
            Self::Default(
                TracingInspector::new(TracingInspectorConfig::from_geth_config(&config)),
                config,
            )
        };

        Ok(this)
    }

    /// Prepares inspector for executing the next transaction. This will remove any state from
    /// previous transactions.
    pub fn fuse(&mut self) -> Result<(), DebugInspectorError> {
        match self {
            Self::FourByte(inspector) => {
                core::mem::take(inspector);
            }
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.fuse(),
            Self::Noop => {}
            Self::Mux(inspector, config) => {
                *inspector = MuxInspector::try_from_config(config.clone())?;
            }
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => {
                *inspector = inspector.try_clone()?.into();
            }
        }

        Ok(())
    }

    /// Should be invoked after each transaction to obtain the resulting [`GethTrace`].
    pub fn get_result<TX, B>(
        &mut self,
        tx_context: Option<TransactionContext>,
        tx_env: &TX,
        block_env: &B,
        result: DebugTraceResult<'_>,
    ) -> Result<GethTrace, DebugInspectorError>
    where
        TX: TraceTxEnv,
        B: TraceBlockEnv,
    {
        #[allow(clippy::needless_update)]
        let tx_info = TransactionInfo {
            hash: tx_context.as_ref().and_then(|c| c.tx_hash),
            index: tx_context.as_ref().and_then(|c| c.tx_index.map(|i| i as u64)),
            block_hash: tx_context.as_ref().and_then(|c| c.block_hash),
            block_number: Some(block_env.trace_block_number()),
            base_fee: Some(block_env.trace_base_fee()),
            ..Default::default()
        };

        let res = match self {
            Self::FourByte(inspector) => FourByteFrame::from(&*inspector).into(),
            Self::CallTracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector.set_transaction_caller(tx_env.trace_caller());
                inspector.geth_builder().geth_call_traces(*config, result.gas_used).into()
            }
            Self::PreStateTracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector
                    .geth_builder()
                    .geth_prestate_traces(result.state, config)
                    .unwrap_or_else(|err| match err {})
                    .into()
            }
            Self::Noop => NoopFrame::default().into(),
            Self::Mux(inspector, _) => inspector
                .try_into_mux_frame(result.gas_used, result.state, tx_info)
                .unwrap_or_else(|err| match err {})
                .into(),
            Self::FlatCallTracer(inspector) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector.set_transaction_caller(tx_env.trace_caller());
                inspector
                    .clone()
                    .into_parity_builder()
                    .into_localized_transaction_traces(tx_info)
                    .into()
            }
            Self::Erc7562Tracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector.set_transaction_caller(tx_env.trace_caller());
                inspector.geth_builder().geth_erc7562_traces(config.clone(), result.gas_used).into()
            }
            Self::Default(inspector, config) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector.set_transaction_caller(tx_env.trace_caller());
                inspector
                    .geth_builder()
                    .geth_traces(result.gas_used, result.return_value.clone(), *config)
                    .into()
            }
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => {
                inspector.set_transaction_context(tx_context.unwrap_or_default());
                let empty_db;
                let db = if let Some(db) = result.db {
                    db
                } else {
                    empty_db = CacheDB::new(EmptyDB::default());
                    &empty_db
                };
                let result = crate::tracing::js::JsTraceResult {
                    success: result.status,
                    gas_used: result.gas_used,
                    stop: result.stop,
                    output: result.return_value.clone(),
                    created_address: result.created_address,
                };
                let tx = crate::tracing::js::JsTraceTx {
                    caller: tx_env.trace_caller(),
                    kind: tx_env.trace_kind(),
                    input: tx_env.trace_input(),
                    gas_limit: tx_env.trace_gas_limit(),
                    gas_price: tx_env.trace_gas_price(),
                    value: tx_env.trace_value(),
                };
                let block = crate::tracing::js::JsTraceBlock {
                    number: block_env.trace_block_number(),
                    coinbase: block_env.trace_coinbase(),
                    timestamp: block_env.trace_timestamp(),
                };
                GethTrace::JS(inspector.json_result_from_parts(result, tx, block, db)?)
            }
        };

        Ok(res)
    }
}

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for DebugInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
        match self {
            Self::FourByte(inspector) => inspector.initialize_interp(interp, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.initialize_interp(interp, host),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.initialize_interp(interp, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.initialize_interp(interp, host),
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
        match self {
            Self::FourByte(inspector) => inspector.step(interp, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.step(interp, host),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.step(interp, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.step(interp, host),
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
        match self {
            Self::FourByte(inspector) => inspector.step_end(interp, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.step_end(interp, host),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.step_end(interp, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.step_end(interp, host),
        }
    }

    fn log(&mut self, log: &Log, host: &mut T::Host) {
        match self {
            Self::FourByte(inspector) => {
                <FourByteInspector as Inspector<T>>::log(inspector, log, host)
            }
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => {
                <TracingInspector as Inspector<T>>::log(inspector, log, host);
            }
            Self::Noop => {}
            Self::Mux(inspector, _) => <MuxInspector as Inspector<T>>::log(inspector, log, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => {
                <crate::tracing::js::JsInspector as Inspector<T>>::log(inspector, log, host);
            }
        }
    }

    fn call(&mut self, message: &mut Message<T>, host: &mut T::Host) -> Option<MessageResult<T>> {
        match self {
            Self::FourByte(inspector) => inspector.call(message, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.call(message, host),
            Self::Noop => None,
            Self::Mux(inspector, _) => inspector.call(message, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.call(message, host),
        }
    }

    fn call_end(
        &mut self,
        message: &Message<T>,
        result: &mut MessageResult<T>,
        host: &mut T::Host,
    ) {
        match self {
            Self::FourByte(inspector) => inspector.call_end(message, result, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.call_end(message, result, host),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.call_end(message, result, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.call_end(message, result, host),
        }
    }

    fn create(&mut self, message: &mut Message<T>, host: &mut T::Host) -> Option<MessageResult<T>> {
        match self {
            Self::FourByte(inspector) => inspector.create(message, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.create(message, host),
            Self::Noop => None,
            Self::Mux(inspector, _) => inspector.create(message, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.create(message, host),
        }
    }

    fn create_end(
        &mut self,
        message: &Message<T>,
        result: &mut MessageResult<T>,
        host: &mut T::Host,
    ) {
        match self {
            Self::FourByte(inspector) => inspector.create_end(message, result, host),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.create_end(message, result, host),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.create_end(message, result, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.create_end(message, result, host),
        }
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host,
    ) {
        match self {
            Self::FourByte(inspector) => {
                <FourByteInspector as Inspector<T>>::selfdestruct(
                    inspector, contract, target, value, host,
                );
            }
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => {
                <TracingInspector as Inspector<T>>::selfdestruct(
                    inspector, contract, target, value, host,
                );
            }
            Self::Noop => {}
            Self::Mux(inspector, _) => {
                <MuxInspector as Inspector<T>>::selfdestruct(
                    inspector, contract, target, value, host,
                );
            }
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => {
                <crate::tracing::js::JsInspector as Inspector<T>>::selfdestruct(
                    inspector, contract, target, value, host,
                );
            }
        }
    }
}

/// Error type for [DebugInspector]
#[derive(Debug, Error)]
pub enum DebugInspectorError {
    /// Invalid tracer configuration
    #[error("invalid tracer config")]
    InvalidTracerConfig,
    /// Unsupported tracer
    #[error("unsupported tracer")]
    UnsupportedTracer,
    /// JS tracer is not enabled
    #[error("JS Tracer is not enabled")]
    JsTracerNotEnabled,
    /// Error from MuxInspector
    #[error(transparent)]
    MuxInspector(#[from] crate::tracing::MuxError),
    /// Error from JS inspector
    #[cfg(feature = "js-tracer")]
    #[error(transparent)]
    JsInspector(#[from] crate::tracing::js::JsInspectorError),
}
