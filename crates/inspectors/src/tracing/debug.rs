use crate::tracing::{
    FourByteInspector, KECCAK256_PREIMAGE_TRACER, Keccak256PreimageInspector, MuxInspector,
    TracingInspector, TracingInspectorConfig, TransactionContext, final_refunded,
    utils::transaction_info,
};
use alloc::vec::Vec;
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{Address, Log, U256};
use alloy_rpc_types_trace::geth::{
    CallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerType,
    GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace, NoopFrame, PreStateConfig,
    StateGasTrace, erc7562::Erc7562Config, mux::MuxConfig,
};
use evm2::{
    DatabaseError, EvmTypes, EvmTypesHost, Inspector, NoopInspector, TxResultWithState,
    env::BlockEnv,
    evm::DynDatabase,
    interpreter::{Interpreter, Message, MessageResult},
};
use thiserror::Error;

#[cfg(feature = "js-tracer")]
use alloc::boxed::Box;

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
    /// EIP-8037 state-gas tracer
    StateGasTracer(NoopInspector),
    /// Mux tracer
    Mux(MuxInspector, MuxConfig),
    /// FlatCallTracer
    FlatCallTracer(TracingInspector),
    /// Erc7562Tracer
    Erc7562Tracer(TracingInspector, Erc7562Config),
    /// Keccak256PreimageTracer
    Keccak256Preimage(Keccak256PreimageInspector),
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
            Self::StateGasTracer(inspector) => Self::StateGasTracer(inspector.clone()),
            Self::Mux(inspector, config) => Self::Mux(inspector.try_clone()?, config.clone()),
            Self::FlatCallTracer(inspector) => Self::FlatCallTracer(inspector.clone()),
            Self::Erc7562Tracer(inspector, config) => {
                Self::Erc7562Tracer(inspector.clone(), config.clone())
            }
            Self::Keccak256Preimage(inspector) => Self::Keccak256Preimage(inspector.clone()),
            Self::Default(inspector, config) => Self::Default(inspector.clone(), *config),
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => Self::Js(inspector.try_clone()?.into()),
        })
    }

    /// Create a new `DebugInspector` from the given tracing options.
    pub fn new(opts: GethDebugTracingOptions) -> Result<Self, DebugInspectorError> {
        let GethDebugTracingOptions { config, tracer, tracer_config, .. } = opts;

        // Normalize an empty tracer name to None so it selects DebugInspector::Default below,
        // the opcode logger that produces structLogs, just like an absent tracer name.
        // See https://github.com/ethereum/execution-apis/blob/main/src/schemas/opcode-tracer.yaml
        let tracer = tracer.filter(|tracer| !tracer.as_str().is_empty());

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
                    GethDebugBuiltInTracerType::StateGasTracer => {
                        Self::StateGasTracer(NoopInspector::default())
                    }
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
                // geth looks a tracer up by name before it evaluates the string as JavaScript.
                GethDebugTracerType::JsTracer(name) if name == KECCAK256_PREIMAGE_TRACER => {
                    Self::Keccak256Preimage(Keccak256PreimageInspector::default())
                }
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
            Self::Keccak256Preimage(inspector) => inspector.clear(),
            Self::Noop(_) | Self::StateGasTracer(_) => {}
            Self::Mux(inspector, _) => inspector.fuse()?,
            #[cfg(feature = "js-tracer")]
            Self::Js(inspector) => inspector.fuse()?,
        }

        Ok(())
    }

    /// Sets the handle that interrupts the JavaScript tracers of this inspector, including those
    /// in a mux tracer, see [`JsInspector::with_interrupt`](crate::tracing::js::JsInspector).
    #[cfg(feature = "js-tracer")]
    pub fn set_js_interrupt(&mut self, interrupt: &crate::tracing::js::JsInspectorInterrupt) {
        match self {
            Self::Js(inspector) => inspector.set_interrupt(interrupt.clone()),
            Self::Mux(inspector, _) => inspector.set_js_interrupt(interrupt),
            _ => {}
        }
    }

    /// Sets the index the next recorded log receives, see
    /// [`TracingInspector::set_next_log_index`].
    pub fn set_next_log_index(&mut self, index: usize) {
        match self {
            Self::CallTracer(inspector, _)
            | Self::PreStateTracer(inspector, _)
            | Self::FlatCallTracer(inspector)
            | Self::Erc7562Tracer(inspector, _)
            | Self::Default(inspector, _) => inspector.set_next_log_index(index),
            Self::Mux(inspector, _) => inspector.set_next_log_index(index),
            _ => {}
        }
    }

    /// Should be invoked after each transaction to obtain the resulting [`GethTrace`].
    ///
    /// Call traces, including those in a mux trace, recover the transaction root when runtime
    /// gas charging halts before evm2 invokes the call/create hooks. Recovered roots report
    /// `out of gas`; fork-specific fields remain omitted when the EVM features were not recorded.
    ///
    /// The default struct logger moves the recorded stack snapshots into the result, so calling
    /// this again before [`Self::fuse`] returns struct logs without stacks.
    pub fn get_result<T: EvmTypesHost<Tx: Transaction>>(
        &mut self,
        tx_context: Option<TransactionContext>,
        tx: &Recovered<T::Tx>,
        block_env: &BlockEnv<T>,
        res: &TxResultWithState<T>,
        db: &mut dyn DynDatabase,
    ) -> Result<GethTrace, DebugInspectorError> {
        let tx_info = transaction_info::<T>(tx_context.as_ref(), block_env);

        let res = match self {
            Self::FourByte(inspector) => FourByteFrame::from(&*inspector).into(),
            Self::CallTracer(inspector, config) => {
                inspector.recover_runtime_oog_root(tx, &res.result);
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.signer());
                inspector
                    .geth_builder()
                    .geth_call_traces_with_result_gas(*config, &res.result)
                    .into()
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
            Self::StateGasTracer(_) => StateGasTrace {
                gas_used: res.result.tx_gas_used(),
                execution_gas_used: res.result.execution_gas_spent(),
                state_gas_used: res.result.state_gas_spent(),
                gas_refund: final_refunded(&res.result),
            }
            .into(),
            Self::Mux(inspector, _) => {
                inspector.try_into_mux_frame(res, tx, block_env, tx_context, db)?.into()
            }
            Self::FlatCallTracer(inspector) => {
                inspector.recover_runtime_oog_root(tx, &res.result);
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.signer());
                inspector
                    .clone()
                    .into_parity_builder()
                    .with_transaction_gas_used(res.result.tx_gas_used())
                    .into_localized_transaction_traces(tx_info)
                    .into()
            }
            Self::Erc7562Tracer(inspector, config) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.signer());
                inspector
                    .geth_builder()
                    .geth_erc7562_traces(config.clone(), res.result.tx_gas_used(), db)
                    .map_err(DebugInspectorError::Database)?
                    .into()
            }
            Self::Keccak256Preimage(inspector) => GethTrace::JS(inspector.json_result()?),
            Self::Default(inspector, config) => {
                inspector.set_transaction_gas_limit(tx.gas_limit());
                inspector.set_transaction_caller(tx.signer());
                // Move the stack snapshots into the struct logs instead of copying them.
                let mut stacks = config.is_stack_enabled().then(|| {
                    inspector
                        .traces_mut()
                        .nodes_mut()
                        .iter_mut()
                        .map(|node| {
                            let stacks = node.trace.steps.iter_mut().map(|step| step.stack.take());
                            stacks.collect::<Vec<_>>().into_iter()
                        })
                        .collect::<Vec<_>>()
                });
                let gas = &res.result;
                inspector
                    .geth_builder()
                    .geth_traces_inner(
                        gas.tx_gas_used(),
                        Some((
                            gas.execution_gas_spent(),
                            gas.state_gas_spent(),
                            final_refunded(gas),
                        )),
                        res.result.output.clone(),
                        *config,
                        stacks.as_deref_mut(),
                    )
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
            Self::Keccak256Preimage($insp) => Inspector::<T>::$method($insp, $($arg),*),
            Self::Noop($insp) => Inspector::<T>::$method($insp, $($arg),*),
            Self::StateGasTracer($insp) => Inspector::<T>::$method($insp, $($arg),*),
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
    /// The keccak256 preimage tracer reached its limit of recorded bytes
    #[error(transparent)]
    Keccak256Preimage(#[from] crate::tracing::Keccak256PreimageLimitExceeded),
    /// Error from JS inspector
    #[cfg(feature = "js-tracer")]
    #[error(transparent)]
    JsInspector(#[from] crate::tracing::js::JsInspectorError),
    /// Database operation failed
    #[error("database error {0:?}")]
    Database(DatabaseError),
}
