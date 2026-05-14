//! EVM tracing inspectors.

use crate::{
    opcode::immediate_size,
    tracing::{
        arena::PushTraceKind,
        types::{
            CallKind, CallLog, CallTrace, CallTraceStep, RecordedMemory, StorageChange,
            StorageChangeReason, TraceMemberOrder,
        },
    },
};
use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use evm2::{
    EvmTypes, Inspector, SpecId,
    bytecode::opcode::{OpCode, op},
    evm::StateChanges,
    interpreter::{Interpreter, Message, MessageKind, MessageResult},
};

mod arena;
pub use arena::CallTraceArena;

mod builder;
pub use builder::{
    geth::{self, GethTraceBuilder},
    parity::{self, ParityTraceBuilder},
};

mod config;
pub use config::{OpcodeFilter, StackSnapshotType, TracingInspectorConfig};

mod fourbyte;
pub use fourbyte::FourByteInspector;

mod opcount;
pub use opcount::OpcodeCountInspector;

pub mod types;

mod utils;

#[cfg(feature = "std")]
mod writer;
#[cfg(feature = "std")]
pub use writer::{TraceWriter, TraceWriterConfig};

/// JavaScript tracing support.
#[cfg(feature = "js-tracer")]
#[allow(dead_code)]
pub mod js {
    use alloc::string::String;
    use boa_engine::{Context, JsError, JsObject, JsValue, Source, js_string};

    pub(crate) mod bindings;
    pub(crate) mod builtins;

    use builtins::register_builtins;

    /// The maximum number of iterations in a loop.
    ///
    /// Once exceeded, the loop will throw an error.
    pub const LOOP_ITERATION_LIMIT: u64 = 200_000;

    /// The recursion limit for function calls.
    ///
    /// Once exceeded, the loop will throw an error.
    pub const RECURSION_LIMIT: usize = 10_000;

    /// A javascript inspector that will delegate inspector functions to javascript functions
    ///
    /// See also <https://geth.ethereum.org/docs/developers/evm-tracing/custom-tracer#custom-javascript-tracing>
    #[derive(Debug)]
    pub struct JsInspector {
        ctx: Context,
        code: String,
        _js_config_value: JsValue,
        config: serde_json::Value,
        obj: JsObject,
        result_fn: JsObject,
        fault_fn: JsObject,
        enter_fn: Option<JsObject>,
        exit_fn: Option<JsObject>,
        step_fn: Option<JsObject>,
    }

    impl JsInspector {
        /// Creates a new inspector from a javascript code snipped that evaluates to an object with
        /// the expected fields and a config object.
        ///
        /// The object must have the following fields:
        ///  - `result`: a function that will be called when the result is requested.
        ///  - `fault`: a function that will be called when the transaction fails.
        ///
        /// Optional functions are invoked during inspection:
        /// - `setup`: a function that will be called before the inspection starts.
        /// - `enter`: a function that will be called when the execution enters a new call.
        /// - `exit`: a function that will be called when the execution exits a call.
        /// - `step`: a function that will be called when the execution steps to the next
        ///   instruction.
        pub fn new(code: String, config: serde_json::Value) -> Result<Self, JsInspectorError> {
            let mut ctx = Context::default();

            ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
            ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);

            register_builtins(&mut ctx)?;

            let wrapped = alloc::format!("({code})");
            let obj = ctx
                .eval(Source::from_bytes(wrapped.as_bytes()))
                .map_err(JsInspectorError::EvalCode)?;

            let obj = obj.as_object().ok_or(JsInspectorError::ExpectedJsObject)?;

            let result_fn = obj
                .get(js_string!("result"), &mut ctx)?
                .as_object()
                .ok_or(JsInspectorError::ResultFunctionMissing)?;
            if !result_fn.is_callable() {
                return Err(JsInspectorError::ResultFunctionMissing);
            }

            let fault_fn = obj
                .get(js_string!("fault"), &mut ctx)?
                .as_object()
                .ok_or(JsInspectorError::FaultFunctionMissing)?;
            if !fault_fn.is_callable() {
                return Err(JsInspectorError::FaultFunctionMissing);
            }

            let enter_fn =
                obj.get(js_string!("enter"), &mut ctx)?.as_object().filter(|o| o.is_callable());
            let exit_fn =
                obj.get(js_string!("exit"), &mut ctx)?.as_object().filter(|o| o.is_callable());
            let step_fn =
                obj.get(js_string!("step"), &mut ctx)?.as_object().filter(|o| o.is_callable());

            let _js_config_value = JsValue::from_json(&config, &mut ctx)
                .map_err(JsInspectorError::InvalidJsonConfig)?;

            if let Some(setup_fn) = obj.get(js_string!("setup"), &mut ctx)?.as_object() {
                if !setup_fn.is_callable() {
                    return Err(JsInspectorError::SetupFunctionNotCallable);
                }

                setup_fn
                    .call(&(obj.clone().into()), core::slice::from_ref(&_js_config_value), &mut ctx)
                    .map_err(JsInspectorError::SetupCallFailed)?;
            }

            Ok(Self {
                ctx,
                code,
                _js_config_value,
                config,
                obj,
                result_fn,
                fault_fn,
                enter_fn,
                exit_fn,
                step_fn,
            })
        }
    }

    /// Error variants that can occur during JavaScript inspection.
    #[derive(Debug, thiserror::Error)]
    pub enum JsInspectorError {
        /// Error originating from a JavaScript operation.
        #[error(transparent)]
        JsError(#[from] JsError),

        /// Failure during the evaluation of JavaScript code.
        #[error("failed to evaluate JS code: {0}")]
        EvalCode(JsError),

        /// The evaluated code is not a JavaScript object.
        #[error("the evaluated code is not a JS object")]
        ExpectedJsObject,

        /// The trace object must expose a function named `result()`.
        #[error("trace object must expose a function result()")]
        ResultFunctionMissing,

        /// The trace object must expose a function named `fault()`.
        #[error("trace object must expose a function fault()")]
        FaultFunctionMissing,

        /// The setup object must be a callable function.
        #[error("setup object must be a function")]
        SetupFunctionNotCallable,

        /// Failure during the invocation of the `setup()` function.
        #[error("failed to call setup(): {0}")]
        SetupCallFailed(JsError),

        /// Invalid JSON configuration encountered.
        #[error("invalid JSON config: {0}")]
        InvalidJsonConfig(JsError),
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_loop_iteration_limit() {
            let mut context = Context::default();
            context.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);

            let code = "let i = 0; while (i++ < 69) {}";
            let result = context.eval(Source::from_bytes(code));
            assert!(result.is_ok());

            let code = "while (true) {}";
            let result = context.eval(Source::from_bytes(code));
            assert!(result.is_err());
        }

        #[test]
        fn test_fault_fn_not_callable() {
            let code = r#"
            {
                result: function() {},
                fault: {},
            }
        "#;
            let config = serde_json::Value::Null;
            let result = JsInspector::new(code.to_string(), config);
            assert!(matches!(result, Err(JsInspectorError::FaultFunctionMissing)));
        }
    }
}

mod mux;
pub use mux::{Error as MuxError, MuxInspector};

mod debug;
pub use debug::{DebugInspector, DebugInspectorError, DebugTraceResult, TraceBlockEnv, TraceTxEnv};

/// An inspector that collects call traces.
#[derive(Clone, Debug)]
pub struct TracingInspector {
    config: TracingInspectorConfig,
    traces: CallTraceArena,
    trace_stack: Vec<usize>,
    step_stack: Vec<(usize, usize, u64, usize)>,
    log_index: u64,
    spec_id: Option<SpecId>,
}

impl Default for TracingInspector {
    fn default() -> Self {
        Self::new(TracingInspectorConfig::default())
    }
}

impl TracingInspector {
    /// Returns a new instance for the given config.
    pub fn new(config: TracingInspectorConfig) -> Self {
        Self {
            config,
            traces: CallTraceArena::default(),
            trace_stack: Vec::new(),
            step_stack: Vec::new(),
            log_index: 0,
            spec_id: None,
        }
    }

    /// Resets the inspector to its initial state.
    pub fn fuse(&mut self) {
        self.traces.clear();
        self.trace_stack.clear();
        self.step_stack.clear();
        self.log_index = 0;
    }

    /// Resets the inspector to its initial state.
    pub fn fused(mut self) -> Self {
        self.fuse();
        self
    }

    /// Returns the config of the inspector.
    pub const fn config(&self) -> &TracingInspectorConfig {
        &self.config
    }

    /// Returns a mutable reference to the config of the inspector.
    pub const fn config_mut(&mut self) -> &mut TracingInspectorConfig {
        &mut self.config
    }

    /// Updates the config of the inspector.
    pub fn update_config(
        &mut self,
        f: impl FnOnce(TracingInspectorConfig) -> TracingInspectorConfig,
    ) {
        self.config = f(self.config);
    }

    /// Gets a reference to the recorded call traces.
    pub const fn traces(&self) -> &CallTraceArena {
        &self.traces
    }

    /// Gets a mutable reference to the recorded call traces.
    pub const fn traces_mut(&mut self) -> &mut CallTraceArena {
        &mut self.traces
    }

    /// Consumes the inspector and returns the recorded call traces.
    pub fn into_traces(self) -> CallTraceArena {
        self.traces
    }

    /// Sets the root transaction gas used.
    #[inline]
    pub fn set_transaction_gas_used(&mut self, gas_used: u64) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.gas_used = gas_used;
        }
    }

    /// Sets the root transaction gas limit.
    #[inline]
    pub fn set_transaction_gas_limit(&mut self, gas_limit: u64) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.gas_limit = gas_limit;
        }
    }

    /// Sets the root transaction caller.
    #[inline]
    pub fn set_transaction_caller(&mut self, caller: Address) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.caller = caller;
        }
    }

    /// Sets the root transaction gas used and returns the inspector.
    #[inline]
    pub fn with_transaction_gas_used(mut self, gas_used: u64) -> Self {
        self.set_transaction_gas_used(gas_used);
        self
    }

    /// Fills storage changes from transaction state changes and recorded SSTORE stack values.
    pub fn fill_storage_changes(&mut self, state: &StateChanges) {
        let mut current_storage = BTreeMap::new();
        for (&address, storage) in &state.storage {
            for (&key, slot) in &storage.slots {
                current_storage.insert((address, key), slot.original);
            }
        }

        let mut changes = Vec::new();
        self.collect_sstore_changes(0, &mut changes);

        for (node_idx, step_idx, address, key, value) in changes {
            let Some(had_value) = current_storage.get_mut(&(address, key)) else {
                continue;
            };
            let step = &mut self.traces.arena[node_idx].trace.steps[step_idx];
            step.storage_change = Some(Box::new(StorageChange {
                key,
                value,
                had_value: Some(*had_value),
                reason: StorageChangeReason::SSTORE,
            }));
            *had_value = value;
        }
    }

    fn collect_sstore_changes(
        &self,
        node_idx: usize,
        changes: &mut Vec<(usize, usize, Address, U256, U256)>,
    ) {
        let Some(node) = self.traces.arena.get(node_idx) else {
            return;
        };
        let address = node.execution_address();

        for order in &node.ordering {
            match *order {
                TraceMemberOrder::Step(step_idx) => {
                    let Some(step) = node.trace.steps.get(step_idx) else {
                        continue;
                    };
                    if step.op.get() != op::SSTORE || step.storage_change.is_some() {
                        continue;
                    }
                    let Some(stack) = &step.stack else {
                        continue;
                    };
                    let Some((key, value)) = stack
                        .split_last()
                        .and_then(|(&key, stack)| stack.last().map(|value| (key, *value)))
                    else {
                        continue;
                    };
                    changes.push((node_idx, step_idx, address, key, value));
                }
                TraceMemberOrder::Call(child_idx) => {
                    let Some(child_idx) = node.children.get(child_idx).copied() else {
                        continue;
                    };
                    self.collect_sstore_changes(child_idx, changes);
                }
                TraceMemberOrder::Log(_) => {}
            }
        }
    }

    /// Returns a geth trace builder over the recorded traces.
    #[inline]
    pub fn geth_builder(&self) -> GethTraceBuilder<'_> {
        GethTraceBuilder::new_borrowed(self.traces.nodes(), self.spec_id)
    }

    /// Consumes the inspector and returns a geth trace builder.
    #[inline]
    pub fn into_geth_builder(self) -> GethTraceBuilder<'static> {
        GethTraceBuilder::new(self.traces.into_nodes(), self.spec_id)
    }

    /// Consumes the inspector and returns a parity trace builder.
    #[inline]
    pub fn into_parity_builder(self) -> ParityTraceBuilder {
        ParityTraceBuilder::new(self.traces.into_nodes(), self.spec_id, self.config)
    }

    fn start_trace<T: EvmTypes>(&mut self, message: &Message<T>) {
        let caller = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => message.destination,
            _ => message.caller,
        };
        let address = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => message.code_address,
            _ => message.destination,
        };
        let trace = CallTrace {
            depth: usize::from(message.depth),
            caller,
            address,
            maybe_precompile: None,
            kind: message.kind.into(),
            value: message.value,
            data: message.input.clone(),
            gas_limit: message.gas_limit,
            ..Default::default()
        };

        let entry = self.trace_stack.last().copied().unwrap_or_default();
        let idx = self.traces.push_trace(entry, PushTraceKind::PushAndAttachToParent, trace);
        self.trace_stack.push(idx);
    }

    fn end_trace<T: EvmTypes>(&mut self, result: &MessageResult<T>) {
        let Some(idx) = self.trace_stack.pop() else {
            return;
        };
        let trace = &mut self.traces.arena[idx].trace;
        trace.status = Some(result.stop);
        trace.success = result.stop.is_success();
        trace.output = result.output.clone();
        trace.gas_used = result.gas.spent();
        trace.gas_refund_counter = result.gas.refunded().max(0) as u64;
        if let Some(address) = result.created_address {
            trace.address = address;
        }
    }
}

impl From<MessageKind> for CallKind {
    fn from(kind: MessageKind) -> Self {
        match kind {
            MessageKind::Call => Self::Call,
            MessageKind::StaticCall => Self::StaticCall,
            MessageKind::DelegateCall => Self::DelegateCall,
            MessageKind::CallCode => Self::CallCode,
            MessageKind::Create => Self::Create,
            MessageKind::Create2 => Self::Create2,
            _ => Self::Call,
        }
    }
}

impl<T: EvmTypes> Inspector<T> for TracingInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        self.spec_id = Some(interp.spec());
        if self.trace_stack.is_empty() {
            self.start_trace(interp.message());
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        self.spec_id = Some(interp.spec());
        if !self.config.record_steps {
            return;
        }
        let Some(trace_idx) = self.trace_stack.last().copied() else {
            return;
        };
        let op = OpCode::new_or_unknown(interp.opcode());
        if !self.config.should_record_opcode(op) {
            return;
        }

        let stack = if self.config.record_stack_snapshots.is_full()
            || self.config.record_stack_snapshots.is_all()
        {
            Some(Box::from(interp.stack().as_slice()))
        } else {
            None
        };
        let memory = self
            .config
            .record_memory_snapshots
            .then(|| RecordedMemory::new(interp.memory_ref().slice(0, interp.memory_ref().len())));
        let returndata = if self.config.record_returndata_snapshots {
            interp.return_data().clone()
        } else {
            Bytes::new()
        };
        let immediate_bytes = if self.config.record_immediate_bytes {
            let immediate_size = usize::from(immediate_size(op.get()));
            (immediate_size > 0).then(|| {
                let pc = interp.pc() + 1;
                let bytecode = interp.bytecode();
                let bytes = bytecode.as_slice().get(pc..pc + immediate_size).unwrap_or_default();
                Bytes::copy_from_slice(bytes)
            })
        } else {
            None
        };
        let step = CallTraceStep {
            pc: interp.pc(),
            op,
            stack,
            push_stack: None,
            memory,
            returndata,
            gas_remaining: interp.gas().remaining(),
            gas_refund_counter: interp.gas().refunded().max(0) as u64,
            gas_used: interp.gas().spent(),
            gas_cost: 0,
            storage_change: None,
            status: None,
            immediate_bytes,
            decoded: None,
        };
        let step_idx = self.traces.arena[trace_idx].trace.steps.len();
        self.traces.arena[trace_idx].ordering.push(TraceMemberOrder::Step(step_idx));
        self.traces.arena[trace_idx].trace.steps.push(step);
        self.step_stack.push((trace_idx, step_idx, interp.gas().remaining(), interp.stack().len()));
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        if !self.config.record_steps {
            return;
        }
        let Some((trace_idx, step_idx, gas_remaining, stack_len_before)) = self.step_stack.pop()
        else {
            return;
        };
        if let Some(step) = self.traces.arena[trace_idx].trace.steps.get_mut(step_idx) {
            step.gas_cost = gas_remaining.saturating_sub(interp.gas().remaining());
            step.status = interp.result().err();
            if self.config.record_stack_snapshots.is_pushes()
                || self.config.record_stack_snapshots.is_all()
            {
                let stack = interp.stack();
                if stack.len() > stack_len_before {
                    step.push_stack = Some(Box::from(&stack.as_slice()[stack_len_before..]));
                }
            }
        }
    }

    fn log(&mut self, log: &Log) {
        if !self.config.record_logs {
            return;
        }
        if let Some(trace_idx) = self.trace_stack.last().copied() {
            let node = &mut self.traces.arena[trace_idx];
            let log_idx = node.log_count();
            node.ordering.push(TraceMemberOrder::Log(log_idx));
            node.logs.push(
                CallLog::from(log.clone()).with_position(log_idx as u64).with_index(self.log_index),
            );
            self.log_index += 1;
        }
    }

    fn call(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        self.start_trace(message);
        None
    }

    fn call_end(&mut self, _message: &Message<T>, result: &mut MessageResult<T>) {
        self.end_trace(result);
    }

    fn create(&mut self, message: &mut Message<T>) -> Option<MessageResult<T>> {
        self.start_trace(message);
        None
    }

    fn create_end(&mut self, _message: &Message<T>, result: &mut MessageResult<T>) {
        self.end_trace(result);
    }

    fn selfdestruct(&mut self, contract: &Address, target: &Address, value: &U256) {
        if let Some(trace_idx) = self.trace_stack.last().copied() {
            let trace = &mut self.traces.arena[trace_idx].trace;
            trace.selfdestruct_address = Some(*contract);
            trace.selfdestruct_refund_target = Some(*target);
            trace.selfdestruct_transferred_value = Some(*value);
        }
    }
}

/// Contextual transaction info made available to debug tracers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionContext {
    /// Hash of the block the transaction is contained within.
    pub block_hash: Option<B256>,
    /// Index of the transaction within a block.
    pub tx_index: Option<usize>,
    /// Hash of the transaction being traced.
    pub tx_hash: Option<B256>,
}

impl TransactionContext {
    /// Sets the block hash.
    pub const fn with_block_hash(mut self, block_hash: B256) -> Self {
        self.block_hash = Some(block_hash);
        self
    }

    /// Sets the index of the transaction within a block.
    pub const fn with_tx_index(mut self, tx_index: usize) -> Self {
        self.tx_index = Some(tx_index);
        self
    }

    /// Sets the transaction hash.
    pub const fn with_tx_hash(mut self, tx_hash: B256) -> Self {
        self.tx_hash = Some(tx_hash);
        self
    }
}

impl From<alloy_rpc_types_eth::TransactionInfo> for TransactionContext {
    fn from(tx_info: alloy_rpc_types_eth::TransactionInfo) -> Self {
        Self {
            block_hash: tx_info.block_hash,
            tx_index: tx_info.index.map(|idx| idx as usize),
            tx_hash: tx_info.hash,
        }
    }
}
