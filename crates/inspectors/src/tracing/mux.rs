use crate::tracing::{
    FourByteInspector, KECCAK256_PREIMAGE_TRACER, Keccak256PreimageInspector,
    Keccak256PreimageLimitExceeded, TracingInspector, TracingInspectorConfig, TransactionContext,
    final_refunded, utils::transaction_info,
};
use alloc::{boxed::Box, string::String, vec::Vec};
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{Address, Log, U256, map::HashMap};
use alloy_rpc_types_trace::geth::{
    CallConfig, FlatCallConfig, FourByteFrame, GethDebugBuiltInTracerType, GethDebugTracerType,
    GethTrace, NoopFrame, PreStateConfig, StateGasTrace,
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
    /// An instance of Keccak256PreimageInspector that can be reused
    keccak256_preimage: Option<Keccak256PreimageInspector>,
    /// An instance of TracingInspector that can be reused
    tracing: Option<TracingInspector>,
    /// Configurations for different Geth trace types
    configs: Vec<(GethDebugBuiltInTracerType, TraceConfig)>,
    /// Tracers that run their own inspector: nested mux tracers and JavaScript tracers
    children: Vec<(GethDebugTracerType, Child)>,
}

/// A tracer of the mux that runs its own inspector.
#[derive(Debug)]
enum Child {
    Mux(Box<MuxInspector>),
    #[cfg(feature = "js-tracer")]
    Js(Box<crate::tracing::js::JsInspector>),
}

impl Child {
    #[cfg(feature = "js-tracer")]
    fn js(code: String, config: serde_json::Value) -> Result<Self, Error> {
        Ok(Self::Js(crate::tracing::js::JsInspector::new(code, config)?.into()))
    }

    #[cfg(not(feature = "js-tracer"))]
    fn js(_code: String, _config: serde_json::Value) -> Result<Self, Error> {
        Err(Error::JsTracerNotEnabled)
    }
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
        let mut keccak256_preimage = None;
        let mut inspector_config = TracingInspectorConfig::none();
        let mut configs = Vec::new();
        let mut children = Vec::new();

        // Process each tracer configuration
        for (tracer_type, tracer_config) in config.0 {
            let builtin = match tracer_type {
                GethDebugTracerType::BuiltInTracer(b) => b,
                GethDebugTracerType::JsTracer(name) if name == KECCAK256_PREIMAGE_TRACER => {
                    keccak256_preimage = Some(Keccak256PreimageInspector::default());
                    continue;
                }
                // geth evaluates a name that is not a built-in tracer as JavaScript.
                GethDebugTracerType::JsTracer(code) => {
                    let config = tracer_config.unwrap_or_default().into_json();
                    children.push((
                        GethDebugTracerType::JsTracer(code.clone()),
                        Child::js(code, config)?,
                    ));
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
                GethDebugBuiltInTracerType::MuxTracer => {
                    let nested = Self::try_from_config(
                        tracer_config.unwrap_or_default().into_mux_config()?,
                    )?;
                    children.push((
                        GethDebugTracerType::BuiltInTracer(builtin),
                        Child::Mux(nested.into()),
                    ));
                }
                _ => {
                    // keep this so that new variants can be supported
                    return Err(Error::UnsupportedTracerType(GethDebugTracerType::BuiltInTracer(
                        builtin,
                    )));
                }
            }
        }

        let tracing = (!configs.is_empty()).then(|| TracingInspector::new(inspector_config));

        Ok(Self { four_byte, keccak256_preimage, tracing, configs, children })
    }

    /// Sets the index the next recorded log receives, see
    /// [`TracingInspector::set_next_log_index`].
    pub fn set_next_log_index(&mut self, index: usize) {
        if let Some(tracing) = &mut self.tracing {
            tracing.set_next_log_index(index);
        }
        for (_, child) in &mut self.children {
            match child {
                Child::Mux(mux) => mux.set_next_log_index(index),
                #[cfg(feature = "js-tracer")]
                Child::Js(_) => {}
            }
        }
    }

    /// Try converting this [MuxInspector] into a [MuxFrame].
    ///
    /// Call traces recover the transaction root when runtime gas charging halts before evm2
    /// invokes the call/create hooks. Recovered roots report `out of gas`.
    pub fn try_into_mux_frame<T: EvmTypesHost<Tx: Transaction>>(
        &mut self,
        result: &TxResultWithState<T>,
        tx: &Recovered<T::Tx>,
        block_env: &BlockEnv<T>,
        tx_context: Option<TransactionContext>,
        db: &mut dyn DynDatabase,
    ) -> Result<MuxFrame, Error> {
        if let Some(inspector) = &mut self.tracing {
            inspector.recover_runtime_oog_root(tx, &result.result);
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
                    gas_refund: final_refunded(&result.result),
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

        // Add keccak256 preimage trace if inspector exists
        if let Some(inspector) = &self.keccak256_preimage {
            frame.insert(
                GethDebugTracerType::JsTracer(KECCAK256_PREIMAGE_TRACER.into()),
                GethTrace::JS(inspector.json_result()?),
            );
        }

        for (tracer_type, child) in &mut self.children {
            let trace = match child {
                Child::Mux(mux) => {
                    mux.try_into_mux_frame(result, tx, block_env, tx_context, db)?.into()
                }
                #[cfg(feature = "js-tracer")]
                Child::Js(js) => {
                    js.set_transaction_context(tx_context.unwrap_or_default());
                    GethTrace::JS(js.json_result(result, tx, block_env, db)?)
                }
            };
            frame.insert(tracer_type.clone(), trace);
        }

        Ok(MuxFrame(frame))
    }
}

impl<T: EvmTypes> Inspector<T> for MuxInspector {
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        for (_, child) in &mut self.children {
            child.initialize_interp(interp);
        }
        if let Some(ref mut inspector) = self.four_byte {
            inspector.initialize_interp(interp);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.initialize_interp(interp);
        }
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        for (_, child) in &mut self.children {
            child.step(interp);
        }
        if let Some(ref mut inspector) = self.four_byte {
            inspector.step(interp);
        }
        if let Some(ref mut inspector) = self.keccak256_preimage {
            inspector.step(interp);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.step(interp);
        }
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        for (_, child) in &mut self.children {
            child.step_end(interp);
        }
        if let Some(ref mut inspector) = self.four_byte {
            inspector.step_end(interp);
        }
        if let Some(ref mut inspector) = self.tracing {
            inspector.step_end(interp);
        }
    }

    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        for (_, child) in &mut self.children {
            Inspector::<T>::log(child, log, host);
        }
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
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        for (_, child) in &mut self.children {
            let _ = child.call(interp, message);
        }
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
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        for (_, child) in &mut self.children {
            child.call_end(interp, message, result);
        }
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
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        for (_, child) in &mut self.children {
            let _ = child.create(interp, message);
        }
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
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        for (_, child) in &mut self.children {
            child.create_end(interp, message, result);
        }
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
        host: &mut T::Host<'_>,
    ) {
        for (_, child) in &mut self.children {
            Inspector::<T>::selfdestruct(child, contract, target, value, host);
        }
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

impl<T: EvmTypes> Inspector<T> for Child {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        match self {
            Self::Mux(mux) => mux.initialize_interp(interp),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.initialize_interp(interp),
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        match self {
            Self::Mux(mux) => mux.step(interp),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.step(interp),
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        match self {
            Self::Mux(mux) => mux.step_end(interp),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.step_end(interp),
        }
    }

    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        match self {
            Self::Mux(mux) => Inspector::<T>::log(&mut **mux, log, host),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => Inspector::<T>::log(&mut **js, log, host),
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        match self {
            Self::Mux(mux) => mux.call(interp, message),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.call(interp, message),
        }
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        match self {
            Self::Mux(mux) => mux.call_end(interp, message, result),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.call_end(interp, message, result),
        }
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        match self {
            Self::Mux(mux) => mux.create(interp, message),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.create(interp, message),
        }
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        match self {
            Self::Mux(mux) => mux.create_end(interp, message, result),
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => js.create_end(interp, message, result),
        }
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        match self {
            Self::Mux(mux) => {
                Inspector::<T>::selfdestruct(&mut **mux, contract, target, value, host)
            }
            #[cfg(feature = "js-tracer")]
            Self::Js(js) => Inspector::<T>::selfdestruct(&mut **js, contract, target, value, host),
        }
    }
}

/// Error type for [MuxInspector]
#[derive(Debug, Error)]
pub enum Error {
    /// JS tracer is not enabled
    #[error("JS Tracer is not enabled")]
    JsTracerNotEnabled,
    /// Error from a JS tracer
    #[cfg(feature = "js-tracer")]
    #[error(transparent)]
    JsInspector(#[from] crate::tracing::js::JsInspectorError),
    /// Unsupported tracer type
    #[error("unsupported tracer type: '{0:?}'")]
    UnsupportedTracerType(GethDebugTracerType),
    /// A tracer reached its limit of recorded bytes
    #[error(transparent)]
    Keccak256Preimage(#[from] Keccak256PreimageLimitExceeded),
    /// Database operation failed
    #[error("database error {0:?}")]
    Database(#[from] DatabaseError),
    /// Error when deserializing the config
    #[error("error deserializing config: {0}")]
    InvalidConfig(#[from] serde_json::Error),
}
