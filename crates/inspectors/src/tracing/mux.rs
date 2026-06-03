use crate::tracing::{FourByteInspector, TracingInspector, TracingInspectorConfig};
use alloc::vec::Vec;
use alloy_primitives::{Address, Log, U256, map::HashMap};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_rpc_types_trace::geth::{
    CallConfig, FlatCallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerType,
    NoopFrame, PreStateConfig,
    mux::{MuxConfig, MuxFrame},
};
use evm2::{
    Evm, EvmTypes, Inspector, TxResult,
    evm::{DbResult, DynDatabase},
    interpreter::{Interpreter, Message, MessageResult},
};
use thiserror::Error;

/// Mux tracing inspector that runs and collects results of multiple inspectors at once.
#[derive(Clone, Debug)]
pub struct MuxInspector {
    /// An instance of FourByteInspector that can be reused
    four_byte: Option<FourByteInspector>,
    /// An instance of TracingInspector that can be reused
    tracing: Option<TracingInspector>,
    /// Configurations for different Geth trace types
    configs: Vec<(GethDebugBuiltInTracerType, TraceConfig)>,
}

/// Holds all Geth supported trace configurations
#[derive(Clone, Debug)]
enum TraceConfig {
    Call(CallConfig),
    PreState(PreStateConfig),
    FlatCall(FlatCallConfig),
    Noop,
}

impl MuxInspector {
    /// Try creating a new instance of [MuxInspector] from the given [MuxConfig].
    pub fn try_from_config(config: MuxConfig) -> Result<Self, Error> {
        let mut four_byte = None;
        let mut inspector_config = TracingInspectorConfig::none();
        let mut configs = Vec::new();

        // Process each tracer configuration
        for (tracer_type, tracer_config) in config.0 {
            let builtin = match tracer_type {
                GethDebugTracerType::BuiltInTracer(b) => b,
                _ => return Err(Error::UnsupportedTracerType(tracer_type)),
            };
            #[allow(unreachable_patterns)]
            match builtin {
                GethDebugBuiltInTracerType::FourByteTracer => {
                    if tracer_config.is_some() {
                        return Err(Error::UnexpectedConfig(builtin));
                    }
                    four_byte = Some(FourByteInspector::default());
                }
                GethDebugBuiltInTracerType::CallTracer => {
                    let call_config =
                        tracer_config.ok_or(Error::MissingConfig(builtin))?.into_call_config()?;

                    inspector_config
                        .merge(TracingInspectorConfig::from_geth_call_config(&call_config));
                    configs.push((builtin, TraceConfig::Call(call_config)));
                }
                GethDebugBuiltInTracerType::PreStateTracer => {
                    let prestate_config = tracer_config
                        .ok_or(Error::MissingConfig(builtin))?
                        .into_pre_state_config()?;

                    inspector_config
                        .merge(TracingInspectorConfig::from_geth_prestate_config(&prestate_config));
                    configs.push((builtin, TraceConfig::PreState(prestate_config)));
                }
                GethDebugBuiltInTracerType::NoopTracer => {
                    if tracer_config.is_some() {
                        return Err(Error::UnexpectedConfig(builtin));
                    }
                    configs.push((builtin, TraceConfig::Noop));
                }
                GethDebugBuiltInTracerType::FlatCallTracer => {
                    let flatcall_config = tracer_config
                        .ok_or(Error::MissingConfig(builtin))?
                        .into_flat_call_config()?;

                    inspector_config
                        .merge(TracingInspectorConfig::from_flat_call_config(&flatcall_config));
                    configs.push((builtin, TraceConfig::FlatCall(flatcall_config)));
                }
                GethDebugBuiltInTracerType::MuxTracer => {
                    return Err(Error::UnexpectedConfig(builtin));
                }
                _ => {
                    // keep this so that new variants can be supported
                    return Err(Error::UnexpectedConfig(builtin));
                }
            }
        }

        let tracing = (!configs.is_empty()).then(|| TracingInspector::new(inspector_config));

        Ok(Self { four_byte, tracing, configs })
    }

    /// Try converting this [MuxInspector] into a [MuxFrame].
    pub fn try_into_mux_frame<T: EvmTypes>(
        &self,
        result: &TxResult<T>,
        tx_info: TransactionInfo,
        db: &mut dyn DynDatabase,
    ) -> DbResult<MuxFrame> {
        let mut frame = HashMap::with_capacity_and_hasher(self.configs.len(), Default::default());

        for (tracer_type, config) in &self.configs {
            let trace = match config {
                TraceConfig::Call(call_config) => {
                    if let Some(inspector) = &self.tracing {
                        inspector
                            .geth_builder()
                            .geth_call_traces(*call_config, result.gas_used)
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
                            .into_localized_transaction_traces(tx_info)
                            .into()
                    } else {
                        continue;
                    }
                }
                TraceConfig::Noop => NoopFrame::default().into(),
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

        Ok(MuxFrame(frame))
    }
}

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for MuxInspector {
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        if let Some(ref mut inspector) = self.four_byte {
            inspector.initialize_interp(interp);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.initialize_interp(interp);
        }
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        if let Some(ref mut inspector) = self.four_byte {
            inspector.step(interp);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.step(interp);
        }
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        if let Some(ref mut inspector) = self.four_byte {
            inspector.step_end(interp);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.step_end(interp);
        }
    }

    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host) {
        if let Some(ref mut inspector) = self.four_byte {
            <FourByteInspector as Inspector<T>>::log(inspector, log, host);
        }
        if let Some(ref mut inspector) = self.tracing {
            <TracingInspector as Inspector<T>>::log(inspector, log, host);
        }
    }

    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        if let Some(ref mut inspector) = self.four_byte {
            let _ = inspector.call(interp, message);
        }
        if let Some(ref mut inspector) = self.tracing {
            return inspector.call(interp, message);
        }
        None
    }

    #[inline]
    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if let Some(ref mut inspector) = self.four_byte {
            inspector.call_end(interp, message, result);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.call_end(interp, message, result);
        }
    }

    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        if let Some(ref mut inspector) = self.four_byte {
            let _ = inspector.create(interp, message);
        }
        if let Some(ref mut inspector) = self.tracing {
            return inspector.create(interp, message);
        }
        None
    }

    #[inline]
    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if let Some(ref mut inspector) = self.four_byte {
            inspector.create_end(interp, message, result);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.create_end(interp, message, result);
        }
    }

    #[inline]
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host,
    ) {
        if let Some(ref mut inspector) = self.four_byte {
            <FourByteInspector as Inspector<T>>::selfdestruct(
                inspector, contract, target, value, host,
            );
        }
        if let Some(ref mut inspector) = self.tracing {
            <TracingInspector as Inspector<T>>::selfdestruct(
                inspector, contract, target, value, host,
            );
        }
    }
}

/// Error type for [MuxInspector]
#[derive(Debug, Error)]
pub enum Error {
    /// Config was provided for a tracer that does not expect it
    #[error("unexpected config for tracer '{0:?}'")]
    UnexpectedConfig(GethDebugBuiltInTracerType),
    /// Expected config is missing
    #[error("expected config is missing for tracer '{0:?}'")]
    MissingConfig(GethDebugBuiltInTracerType),
    /// Unsupported tracer type (e.g. JS tracer)
    #[error("unsupported tracer type: '{0:?}'")]
    UnsupportedTracerType(GethDebugTracerType),
    /// Error when deserializing the config
    #[error("error deserializing config: {0}")]
    InvalidConfig(#[from] serde_json::Error),
}
