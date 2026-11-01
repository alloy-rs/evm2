use crate::tracing::{
    DebugInspector, DebugInspectorError, FourByteInspector, TracingInspector,
    TracingInspectorConfig, TransactionContext, utils::transaction_info,
};
use alloc::{boxed::Box, vec::Vec};
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{Address, Log, U256, map::HashMap};
use alloy_rpc_types_trace::geth::{
    CallConfig, FlatCallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerConfig,
    GethDebugTracerType, GethDebugTracingOptions, NoopFrame, PreStateConfig, StateGasTrace,
    mux::{MuxConfig, MuxFrame},
};
use evm2::{
    DatabaseError, EvmTypes, EvmTypesHost, Inspector, TxResultWithState,
    env::BlockEnv,
    evm::DynDatabase,
    interpreter::{Interpreter, Message, MessageResult},
};
use thiserror::Error;

/// Mux tracing inspector that runs and collects results of multiple inspectors at once.
#[derive(Debug)]
pub struct MuxInspector {
    /// An instance of FourByteInspector that can be reused
    four_byte: Option<FourByteInspector>,
    /// An instance of TracingInspector that can be reused
    tracing: Option<TracingInspector>,
    /// Configurations for different Geth trace types
    configs: Vec<(GethDebugBuiltInTracerType, TraceConfig)>,
    /// Tracers that run their own inspector, keyed by their tracer type.
    children: Vec<(GethDebugTracerType, DebugInspector)>,
}

/// Holds all Geth supported trace configurations
#[derive(Clone, Debug)]
enum TraceConfig {
    Call(CallConfig),
    PreState(PreStateConfig),
    FlatCall(FlatCallConfig),
    StateGas,
    Noop,
}

impl MuxInspector {
    /// Try creating a new instance of [MuxInspector] from the given [MuxConfig].
    pub fn try_from_config(config: MuxConfig) -> Result<Self, Error> {
        let mut four_byte = None;
        let mut inspector_config = TracingInspectorConfig::none();
        let mut configs = Vec::new();
        let mut children = Vec::new();

        // Process each tracer configuration
        for (tracer_type, tracer_config) in config.0 {
            let builtin = match &tracer_type {
                GethDebugTracerType::BuiltInTracer(builtin) => *builtin,
                // geth evaluates a name that is not a built-in tracer as JavaScript.
                GethDebugTracerType::JsTracer(_) => {
                    children.push(Self::child(tracer_type, tracer_config)?);
                    continue;
                }
            };
            #[allow(unreachable_patterns)]
            match builtin {
                GethDebugBuiltInTracerType::FourByteTracer => {
                    four_byte = Some(FourByteInspector::default());
                }
                GethDebugBuiltInTracerType::CallTracer => {
                    let call_config = tracer_config.unwrap_or_default().into_call_config()?;

                    inspector_config
                        .merge(TracingInspectorConfig::from_geth_call_config(&call_config));
                    configs.push((builtin, TraceConfig::Call(call_config)));
                }
                GethDebugBuiltInTracerType::PreStateTracer => {
                    let prestate_config =
                        tracer_config.unwrap_or_default().into_pre_state_config()?;

                    inspector_config
                        .merge(TracingInspectorConfig::from_geth_prestate_config(&prestate_config));
                    configs.push((builtin, TraceConfig::PreState(prestate_config)));
                }
                GethDebugBuiltInTracerType::NoopTracer => {
                    configs.push((builtin, TraceConfig::Noop));
                }
                GethDebugBuiltInTracerType::StateGasTracer => {
                    configs.push((builtin, TraceConfig::StateGas));
                }
                GethDebugBuiltInTracerType::FlatCallTracer => {
                    let flatcall_config =
                        tracer_config.unwrap_or_default().into_flat_call_config()?;

                    inspector_config
                        .merge(TracingInspectorConfig::from_flat_call_config(&flatcall_config));
                    configs.push((builtin, TraceConfig::FlatCall(flatcall_config)));
                }
                GethDebugBuiltInTracerType::MuxTracer
                | GethDebugBuiltInTracerType::Erc7562Tracer => {
                    children.push(Self::child(tracer_type, tracer_config)?);
                }
                _ => {
                    // keep this so that new variants can be supported
                    return Err(Error::UnsupportedTracerType(tracer_type));
                }
            }
        }

        let tracing = (!configs.is_empty()).then(|| TracingInspector::new(inspector_config));

        Ok(Self { four_byte, tracing, configs, children })
    }

    /// Creates the inspector of a tracer that does not share the recording of the mux.
    fn child(
        tracer_type: GethDebugTracerType,
        tracer_config: Option<GethDebugTracerConfig>,
    ) -> Result<(GethDebugTracerType, DebugInspector), Error> {
        let options = GethDebugTracingOptions {
            tracer: Some(tracer_type.clone()),
            tracer_config: tracer_config.unwrap_or_default(),
            ..Default::default()
        };
        Ok((tracer_type, DebugInspector::new(options).map_err(Box::new)?))
    }

    /// Creates a copy of this inspector, with copies of the inspectors of its nested tracers.
    ///
    /// A JavaScript tracer shares its interrupt handle with its copy.
    pub fn try_clone(&self) -> Result<Self, Error> {
        let children = self
            .children
            .iter()
            .map(|(tracer_type, child)| {
                Ok((tracer_type.clone(), child.try_clone().map_err(Box::new)?))
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            four_byte: self.four_byte.clone(),
            tracing: self.tracing.clone(),
            configs: self.configs.clone(),
            children,
        })
    }

    /// Prepares the inspector for executing the next transaction. This removes any state from the
    /// previous transaction.
    pub fn fuse(&mut self) -> Result<(), Error> {
        if let Some(inspector) = &mut self.four_byte {
            core::mem::take(inspector);
        }
        if let Some(inspector) = &mut self.tracing {
            inspector.fuse();
        }
        for (_, child) in &mut self.children {
            child.fuse().map_err(Box::new)?;
        }
        Ok(())
    }

    /// Sets the handle that interrupts the JavaScript tracers of this mux, including those of
    /// nested mux tracers, see [`JsInspector::with_interrupt`](crate::tracing::js::JsInspector).
    #[cfg(feature = "js-tracer")]
    pub fn set_js_interrupt(&mut self, interrupt: &crate::tracing::js::JsInspectorInterrupt) {
        for (_, child) in &mut self.children {
            child.set_js_interrupt(interrupt);
        }
    }

    /// Sets the index the next recorded log receives, see
    /// [`TracingInspector::set_next_log_index`].
    pub fn set_next_log_index(&mut self, index: usize) {
        if let Some(tracing) = &mut self.tracing {
            tracing.set_next_log_index(index);
        }
        for (_, child) in &mut self.children {
            child.set_next_log_index(index);
        }
    }

    /// Try converting this [MuxInspector] into a [MuxFrame].
    pub fn try_into_mux_frame<T: EvmTypesHost<Tx: Transaction>>(
        &mut self,
        result: &TxResultWithState<T>,
        tx: &Recovered<T::Tx>,
        block_env: &BlockEnv<T>,
        tx_context: Option<TransactionContext>,
        db: &mut dyn DynDatabase,
    ) -> Result<MuxFrame, Error> {
        if let Some(inspector) = &mut self.tracing {
            inspector.set_transaction_gas_limit(tx.gas_limit());
            inspector.set_transaction_caller(tx.signer());
        }
        let tx_info = transaction_info::<T>(tx_context.as_ref(), block_env);

        let mut frame = HashMap::with_capacity_and_hasher(self.configs.len(), Default::default());

        for (tracer_type, config) in &self.configs {
            let trace = match config {
                TraceConfig::Call(call_config) => {
                    if let Some(inspector) = &self.tracing {
                        inspector
                            .geth_builder()
                            .geth_call_traces_with_result_gas(*call_config, &result.result)
                            .into()
                    } else {
                        continue;
                    }
                }
                TraceConfig::PreState(prestate_config) => {
                    if let Some(inspector) = &self.tracing {
                        inspector
                            .geth_builder()
                            .geth_prestate_traces(result, prestate_config, db)?
                            .into()
                    } else {
                        continue;
                    }
                }
                TraceConfig::FlatCall(_flatcall_config) => {
                    if let Some(inspector) = &self.tracing {
                        inspector
                            .clone()
                            .into_parity_builder()
                            .with_transaction_gas_used(result.result.tx_gas_used())
                            .into_localized_transaction_traces(tx_info)
                            .into()
                    } else {
                        continue;
                    }
                }
                TraceConfig::Noop => NoopFrame::default().into(),
                TraceConfig::StateGas => StateGasTrace {
                    gas_used: result.result.tx_gas_used(),
                    execution_gas_used: result.result.execution_gas_spent(),
                    state_gas_used: result.result.state_gas_spent(),
                    gas_refund: result.result.final_refunded(),
                }
                .into(),
            };

            frame.insert(GethDebugTracerType::BuiltInTracer(*tracer_type), trace);
        }

        // Add four byte trace if inspector exists
        if let Some(inspector) = &self.four_byte {
            frame.insert(
                GethDebugTracerType::BuiltInTracer(GethDebugBuiltInTracerType::FourByteTracer),
                FourByteFrame::from(inspector).into(),
            );
        }

        for (tracer_type, child) in &mut self.children {
            let trace =
                child.get_result(tx_context, tx, block_env, result, db).map_err(Box::new)?;
            frame.insert(tracer_type.clone(), trace);
        }

        Ok(MuxFrame(frame))
    }
}

/// Calls a hook on every inspector of the mux.
macro_rules! fan_out {
    ($mux:ident.$hook:ident($($arg:expr),*)) => {{
        for (_, child) in &mut $mux.children {
            Inspector::<T>::$hook(child, $($arg),*);
        }
        if let Some(inspector) = &mut $mux.four_byte {
            Inspector::<T>::$hook(inspector, $($arg),*);
        }
        if let Some(inspector) = &mut $mux.tracing {
            Inspector::<T>::$hook(inspector, $($arg),*);
        }
    }};
}

impl<T: EvmTypes> Inspector<T> for MuxInspector {
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        fan_out!(self.initialize_interp(interp));
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        fan_out!(self.step(interp));
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        fan_out!(self.step_end(interp));
    }

    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        fan_out!(self.log(log, host));
    }

    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        for (_, child) in &mut self.children {
            let _ = child.call(interp, message);
        }
        if let Some(inspector) = &mut self.four_byte {
            let _ = inspector.call(interp, message);
        }
        self.tracing.as_mut().and_then(|inspector| inspector.call(interp, message))
    }

    #[inline]
    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        fan_out!(self.call_end(interp, message, result));
    }

    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        for (_, child) in &mut self.children {
            let _ = child.create(interp, message);
        }
        if let Some(inspector) = &mut self.four_byte {
            let _ = inspector.create(interp, message);
        }
        self.tracing.as_mut().and_then(|inspector| inspector.create(interp, message))
    }

    #[inline]
    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        fan_out!(self.create_end(interp, message, result));
    }

    #[inline]
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        fan_out!(self.selfdestruct(contract, target, value, host));
    }
}

/// Error type for [MuxInspector]
#[derive(Debug, Error)]
pub enum Error {
    /// Unsupported tracer type
    #[error("unsupported tracer type: '{0:?}'")]
    UnsupportedTracerType(GethDebugTracerType),
    /// Error when deserializing the config
    #[error("error deserializing config: {0}")]
    InvalidConfig(#[from] serde_json::Error),
    /// Error from the inspector of a nested tracer
    #[error(transparent)]
    Child(#[from] Box<DebugInspectorError>),
    /// Database operation failed
    #[error("database error {0:?}")]
    Database(#[from] DatabaseError),
}
