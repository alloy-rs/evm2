use crate::tracing::{
    FourByteInspector, MuxInspector, TracingInspector, TracingInspectorConfig, TransactionContext,
};
#[cfg(feature = "js-tracer")]
use alloc::boxed::Box;
use alloy_primitives::{Address, Log, U256};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::{
    CallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerType,
    GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace, NoopFrame, PreStateConfig,
    erc7562::Erc7562Config, mux::MuxConfig,
};
use evm2::{
    ErrorCode, EvmTypes, EvmTypesHost, Inspector, NoopInspector, TxResultWithState,
    env::BlockEnv,
    ethereum::TransactionExt,
    evm::DynDatabase,
    interpreter::{Interpreter, Message, MessageResult},
};
use thiserror::Error;

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
    Noop(NoopInspector),
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
            Self::Noop(inspector) => Self::Noop(inspector.clone()),
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
                    GethDebugBuiltInTracerType::NoopTracer => Self::Noop(NoopInspector::default()),
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
                GethDebugTracerType::JsTracer(code) => {
                    let config = tracer_config.into_json();
                    Self::Js(crate::tracing::js::JsInspector::new(code, config)?.into())
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
            Self::Noop(_) => {}
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
    pub fn get_result<T: EvmTypesHost<Tx: TransactionExt>>(
        &mut self,
        tx_context: Option<TransactionContext>,
        tx: &T::Tx,
        block_env: &BlockEnv<T>,
        res: &TxResultWithState<T>,
        db: &mut dyn DynDatabase,
    ) -> Result<GethTrace, DebugInspectorError> {
        #[allow(clippy::needless_update)]
        let tx_info = TransactionInfo {
            hash: tx_context.as_ref().and_then(|c| c.tx_hash),
            index: tx_context.as_ref().and_then(|c| c.tx_index.map(|i| i as u64)),
            block_hash: tx_context.as_ref().and_then(|c| c.block_hash),
            block_number: Some(block_env.number.saturating_to()),
            base_fee: Some(block_env.basefee.saturating_to()),
            ..Default::default()
        };

        let res = match self {
            Self::FourByte(inspector) => FourByteFrame::from(&*inspector).into(),
            Self::CallTracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.caller());
                inspector.geth_builder().geth_call_traces(*config, res.result.tx_gas_used()).into()
            }
            Self::PreStateTracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector
                    .geth_builder()
                    .geth_prestate_traces(res, config, db)
                    .map_err(DebugInspectorError::Database)?
                    .into()
            }
            Self::Noop(_) => NoopFrame::default().into(),
            Self::Mux(inspector, _) => inspector
                .try_into_mux_frame(res, db, tx_info)
                .map_err(DebugInspectorError::Database)?
                .into(),
            Self::FlatCallTracer(inspector) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.caller());
                inspector
                    .clone()
                    .into_parity_builder()
                    .into_localized_transaction_traces(tx_info)
                    .into()
            }
            Self::Erc7562Tracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.caller());
                inspector
                    .geth_builder()
                    .geth_erc7562_traces(config.clone(), res.result.tx_gas_used(), db)
                    .map_err(DebugInspectorError::Database)?
                    .into()
            }
            Self::Default(inspector, config) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.caller());
                inspector
                    .geth_builder()
                    .geth_traces(res.result.tx_gas_used(), res.result.output.clone(), *config)
                    .into()
            }
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => {
                inspector.set_transaction_context(tx_context.unwrap_or_default());
                GethTrace::JS(inspector.json_result(res, tx, block_env, db)?)
            }
        };

        Ok(res)
    }
}

macro_rules! delegate {
    ($self:expr => $insp:ident.$method:ident($($arg:expr),*)) => {
        match $self {
            Self::FourByte($insp) => Inspector::<T>::$method($insp, $($arg),*),
            Self::CallTracer($insp, _) => Inspector::<T>::$method($insp, $($arg),*),
            Self::PreStateTracer($insp, _) => Inspector::<T>::$method($insp, $($arg),*),
            Self::FlatCallTracer($insp) => Inspector::<T>::$method($insp, $($arg),*),
            Self::Erc7562Tracer($insp, _) => Inspector::<T>::$method($insp, $($arg),*),
            Self::Default($insp, _) => Inspector::<T>::$method($insp, $($arg),*),
            Self::Noop($insp) => Inspector::<T>::$method($insp, $($arg),*),
            Self::Mux($insp, _) => Inspector::<T>::$method($insp, $($arg),*),
            #[cfg(feature = "js-tracer")]
            Self::Js($insp) => Inspector::<T>::$method(&mut **$insp, $($arg),*),
        }
    };
}

impl<T: EvmTypes> Inspector<T> for DebugInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        delegate!(self => inspector.initialize_interp(interp))
    }

    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        delegate!(self => inspector.step(interp))
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        delegate!(self => inspector.step_end(interp))
    }

    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        delegate!(self => inspector.log(log, host))
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        delegate!(self => inspector.call(interp, message))
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        delegate!(self => inspector.call_end(interp, message, result))
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        delegate!(self => inspector.create(interp, message))
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        delegate!(self => inspector.create_end(interp, message, result))
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        delegate!(self => inspector.selfdestruct(contract, target, value, host))
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
    /// Database operation failed
    #[error("database error {0:?}")]
    Database(ErrorCode),
}
