use crate::tracing::{
    FourByteInspector, MuxInspector, TracingInspector, TracingInspectorConfig, TransactionContext,
    geth::TraceTransactionResult,
};
use alloy_primitives::{Address, Log, U256};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::{
    CallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerType,
    GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace, NoopFrame, PreStateConfig,
    erc7562::Erc7562Config, mux::MuxConfig,
};
use evm2::{
    EvmTypes, Inspector,
    env::{BlockEnv, TxEnv},
    interpreter::{Interpreter, Message, MessageResult},
};
use thiserror::Error;

/// Transaction fields needed by debug trace finalization.
pub trait TraceTxEnv {
    /// Returns transaction gas limit.
    fn trace_gas_limit(&self) -> u64;

    /// Returns transaction caller.
    fn trace_caller(&self) -> Address;
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

    /// Returns the block base fee.
    fn trace_base_fee(&self) -> u64;
}

impl<T: EvmTypes> TraceBlockEnv for BlockEnv<T> {
    fn trace_block_number(&self) -> u64 {
        self.number.try_into().unwrap_or(u64::MAX)
    }

    fn trace_base_fee(&self) -> u64 {
        self.basefee.try_into().unwrap_or(u64::MAX)
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
                GethDebugTracerType::JsTracer(_) => {
                    return Err(DebugInspectorError::JsTracerNotEnabled);
                }
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
        }

        Ok(())
    }

    /// Should be invoked after each transaction to obtain the resulting [`GethTrace`].
    pub fn get_result<R, TX, B, DB>(
        &mut self,
        tx_context: Option<TransactionContext>,
        tx_env: &TX,
        block_env: &B,
        res: &R,
        db: &mut DB,
    ) -> Result<GethTrace, DebugInspectorError>
    where
        R: TraceTransactionResult,
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
                inspector.geth_builder().geth_call_traces(*config, res.trace_gas_used()).into()
            }
            Self::PreStateTracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector
                    .geth_builder()
                    .geth_prestate_traces(res, config, db)
                    .unwrap_or_else(|err| match err {})
                    .into()
            }
            Self::Noop => NoopFrame::default().into(),
            Self::Mux(inspector, _) => inspector
                .try_into_mux_frame(res, db, tx_info)
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
                inspector
                    .geth_builder()
                    .geth_erc7562_traces(config.clone(), res.trace_gas_used(), db)
                    .into()
            }
            Self::Default(inspector, config) => {
                inspector.set_transaction_gas_limit(tx_env.trace_gas_limit());
                inspector.set_transaction_caller(tx_env.trace_caller());
                inspector
                    .geth_builder()
                    .geth_traces(res.trace_gas_used(), res.trace_output(), *config)
                    .into()
            }
        };

        Ok(res)
    }
}

impl<T: EvmTypes> Inspector<T> for DebugInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        match self {
            Self::FourByte(inspector) => inspector.initialize_interp(interp),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.initialize_interp(interp),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.initialize_interp(interp),
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        match self {
            Self::FourByte(inspector) => inspector.step(interp),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.step(interp),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.step(interp),
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        match self {
            Self::FourByte(inspector) => inspector.step_end(interp),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.step_end(interp),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.step_end(interp),
        }
    }

    fn log(&mut self, log: &Log) {
        match self {
            Self::FourByte(inspector) => <FourByteInspector as Inspector<T>>::log(inspector, log),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => {
                <TracingInspector as Inspector<T>>::log(inspector, log);
            }
            Self::Noop => {}
            Self::Mux(inspector, _) => <MuxInspector as Inspector<T>>::log(inspector, log),
        }
    }

    fn call(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        match self {
            Self::FourByte(inspector) => inspector.call(message),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.call(message),
            Self::Noop => None,
            Self::Mux(inspector, _) => inspector.call(message),
        }
    }

    fn call_end(&mut self, message: &Message<T>, result: &mut MessageResult<T>) {
        match self {
            Self::FourByte(inspector) => inspector.call_end(message, result),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.call_end(message, result),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.call_end(message, result),
        }
    }

    fn create(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        match self {
            Self::FourByte(inspector) => inspector.create(message),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.create(message),
            Self::Noop => None,
            Self::Mux(inspector, _) => inspector.create(message),
        }
    }

    fn create_end(&mut self, message: &Message<T>, result: &mut MessageResult<T>) {
        match self {
            Self::FourByte(inspector) => inspector.create_end(message, result),
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.create_end(message, result),
            Self::Noop => {}
            Self::Mux(inspector, _) => inspector.create_end(message, result),
        }
    }

    fn selfdestruct(&mut self, contract: &Address, target: &Address, value: &U256) {
        match self {
            Self::FourByte(inspector) => {
                <FourByteInspector as Inspector<T>>::selfdestruct(
                    inspector, contract, target, value,
                );
            }
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => {
                <TracingInspector as Inspector<T>>::selfdestruct(
                    inspector, contract, target, value,
                );
            }
            Self::Noop => {}
            Self::Mux(inspector, _) => {
                <MuxInspector as Inspector<T>>::selfdestruct(inspector, contract, target, value);
            }
        }
    }
}

/// Error type for [DebugInspector]
#[derive(Debug, Error)]
pub enum DebugInspectorError<DBError = core::convert::Infallible> {
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
    /// Database error
    #[error("database error: {0}")]
    Database(DBError),
}
