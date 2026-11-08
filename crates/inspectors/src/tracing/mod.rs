//! EVM tracing inspectors.

use crate::{
    opcode::immediate_size,
    tracing::types::{
        CallKind, CallTraceNode, RecordedMemory, StepDelta, StorageChange, StorageChangeReason,
        TraceMemberOrder,
    },
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use alloy_consensus::TxEip8141;
use alloy_eips::eip8141::{ENTRY_POINT, Frame, FrameMode, FrameReceipt, FrameStatus};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use alloy_rpc_types_trace::{geth::FlatCallConfig, parity::StorageDelta};
use core::{mem, ops::Range};
use evm2::{
    EvmFeatures, EvmTypes, Inspector, InspectorConfig, OpcodeSet, SpecId,
    ethereum::eip8141::FrameGas,
    interpreter::{
        InstrStop, Interpreter, Message, MessageKind, MessageResult, MessageResultExt,
        opcode::{OpCode, op},
    },
};

mod arena;
pub use arena::CallTraceArena;

mod builder;
pub use builder::{
    geth::{self, GethTraceBuilder},
    parity::{self, ParityTraceBuilder, PrecompileExclusion},
};

mod config;
pub use config::{StackSnapshotType, TracingInspectorConfig};

mod erc7562;
pub use erc7562::{Erc7562Inspector, Error as Erc7562Error};

mod fourbyte;
pub use fourbyte::FourByteInspector;

mod keccak;
pub use keccak::{
    DEFAULT_MAX_RECORDED_BYTES, KECCAK256_PREIMAGE_TRACER, Keccak256PreimageInspector,
    LimitExceeded as Keccak256PreimageLimitExceeded, MEMORY_PAD_LIMIT,
};

mod limits;
use limits::TraceBudget;
pub use limits::{TraceLimitBehavior, TraceLimits};

mod opcount;
pub use opcount::OpcodeCountInspector;

pub mod types;
use types::{CallLog, CallTrace, CallTraceStep};

mod utils;

#[cfg(feature = "std")]
mod writer;
#[cfg(feature = "std")]
pub use writer::{TraceWriter, TraceWriterConfig};

/// JavaScript tracing support.
#[cfg(feature = "js-tracer")]
#[allow(dead_code)]
pub mod js;

mod mux;
pub use mux::{Error as MuxError, MuxInspector};

mod debug;
pub use debug::{DebugInspector, DebugInspectorError};

/// An inspector that collects call traces.
///
/// This [Inspector] can be hooked into evm2's EVM which then calls the inspector functions, such
/// as [Inspector::call] or [Inspector::call_end].
///
/// The [TracingInspector] keeps track of everything by:
///   1. start tracking steps/calls on [Inspector::step] and [Inspector::call]
///   2. complete steps/calls on [Inspector::step_end] and [Inspector::call_end]
#[derive(Clone, Debug, Default)]
pub struct TracingInspector {
    /// Configures what and how the inspector records traces.
    config: TracingInspectorConfig,
    /// Tracks retained and copied trace bytes.
    budget: TraceBudget,
    /// Records all call traces
    traces: CallTraceArena,
    /// Tracks active calls
    trace_stack: Vec<TraceStackEntry>,
    /// Number of logs recorded so far, used as the index of the next log.
    log_count: usize,
    /// Tracks recorded steps waiting for `step_end`.
    ///
    /// This is a stack because nested calls are executed between the `step` and `step_end` of the
    /// call instruction itself. A `usize::MAX` step index marks a step that was not recorded.
    step_stack: Vec<PendingStep>,
    /// Number of opcode steps captured across all calls since the last reset.
    recorded_steps: u64,
    /// The spec id of the EVM.
    ///
    /// This is filled during execution.
    spec_id: Option<SpecId>,
    /// The active EVM features.
    ///
    /// This is filled during execution.
    features: EvmFeatures,
    /// Active frame-transaction trace bookkeeping.
    frame_transaction: Option<FrameTracing>,
    /// Pool of reusable _empty_ step vectors to reduce allocations.
    ///
    /// All `Vec<CallTraceStep>` are always empty but may have capacity.
    reusable_step_vecs: Vec<Vec<CallTraceStep>>,
}

#[derive(Clone, Debug)]
struct FrameTracing {
    payload: Arc<TxEip8141>,
    active: usize,
    nodes: Vec<Option<usize>>,
    log_start: usize,
}

impl TracingInspector {
    /// Returns a new instance for the given config
    pub fn new(config: TracingInspectorConfig) -> Self {
        Self { config, ..Default::default() }
    }

    /// Configures the byte budget for recorded trace data.
    pub const fn with_limits(mut self, limits: TraceLimits) -> Self {
        self.budget.limits = limits;
        self
    }

    /// Returns the number of retained or copied bytes counted against the budget.
    pub const fn recorded_bytes(&self) -> usize {
        self.budget.recorded
    }

    /// Returns whether recording omitted a byte buffer after reaching the budget.
    pub const fn limit_exceeded(&self) -> bool {
        self.budget.exceeded()
    }

    /// Resets the inspector to its initial state of [Self::new].
    /// This makes the inspector ready to be used again.
    ///
    /// Note that this method has no effect on the allocated capacity of the vector.
    #[inline]
    pub fn fuse(&mut self) {
        let Self {
            traces,
            trace_stack,
            log_count,
            step_stack,
            recorded_steps,
            spec_id,
            features,
            frame_transaction,
            // kept
            config,
            budget,
            reusable_step_vecs,
        } = self;

        // if we record steps we can reuse the individual calltracestep vecs
        if config.record_steps {
            for node in traces.nodes_mut() {
                // move out and store the reusable steps vec
                let mut steps = mem::take(&mut node.trace.steps);
                // ensure steps are cleared
                steps.clear();
                // Synthetic roots and entries without interpreter steps allocate no buffer.
                // Caching those empty Vec headers would grow this pool on every frame reset.
                if steps.capacity() != 0 {
                    reusable_step_vecs.push(steps);
                }
            }
        }

        *frame_transaction = None;
        traces.clear();
        trace_stack.clear();
        *log_count = 0;
        step_stack.clear();
        *recorded_steps = 0;
        spec_id.take();
        *features = EvmFeatures::empty();
        budget.recorded = 0;
        budget.exceeded = false;
    }

    /// Resets the inspector to it's initial state of [Self::new].
    #[inline]
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

    /// Manually set the gas used of the root trace.
    ///
    /// This is useful if the root trace's gasUsed should mirror the actual gas used by the
    /// transaction.
    ///
    /// This allows setting it manually by consuming the execution result's gas for example.
    #[inline]
    pub fn set_transaction_gas_used(&mut self, gas_used: u64) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.gas_used = gas_used;
        }
    }

    /// Manually set the gas limit of the debug root trace.
    ///
    /// This is useful if the debug root trace's gasUsed should mirror the actual gas used by the
    /// transaction.
    ///
    /// This allows setting it manually by consuming the execution result's gas for example.
    #[inline]
    pub fn set_transaction_gas_limit(&mut self, gas_limit: u64) {
        if let Some(node) = self.traces.arena.first_mut()
            && !node.trace.frame_transaction_root
        {
            node.trace.gas_limit = gas_limit;
        }
    }

    /// Sets the index the next recorded log receives.
    ///
    /// Log indices count all logs that took effect in the block, so this should be the number of
    /// logs emitted by the preceding transactions when tracing a transaction inside a block.
    /// [`Self::fuse`] clears it, so set it again before each transaction.
    #[inline]
    pub const fn set_next_log_index(&mut self, index: usize) {
        self.log_count = index;
    }

    /// Convenience function for [ParityTraceBuilder::set_transaction_gas_used] that consumes the
    /// type.
    #[inline]
    pub fn with_transaction_gas_used(mut self, gas_used: u64) -> Self {
        self.set_transaction_gas_used(gas_used);
        self
    }

    /// Work with [TracingInspector::set_transaction_gas_limit] function
    #[inline]
    pub fn with_transaction_gas_limit(mut self, gas_limit: u64) -> Self {
        self.set_transaction_gas_limit(gas_limit);
        self
    }

    /// Manually set the caller address of the root trace.
    ///
    /// This is useful for custom transaction types (e.g. account abstraction batches) where the
    /// EVM's call entry point may not reflect the actual transaction sender.
    #[inline]
    pub fn set_transaction_caller(&mut self, caller: Address) {
        if let Some(node) = self.traces.arena.first_mut()
            && !node.trace.frame_transaction_root
        {
            node.trace.caller = caller;
        }
    }

    /// Consumes the Inspector and returns a [ParityTraceBuilder] that leaves out calls to
    /// precompiles without value, see [PrecompileExclusion::ZeroValue].
    #[inline]
    pub fn into_parity_builder(self) -> ParityTraceBuilder {
        ParityTraceBuilder::new(self.traces.arena, PrecompileExclusion::ZeroValue)
    }

    /// Consumes the Inspector and returns a [ParityTraceBuilder] for geth's flatCallTracer.
    ///
    /// Unlike [Self::into_parity_builder], this keeps or leaves out calls to precompiles as the
    /// tracer's `includePrecompiles` option says.
    #[inline]
    pub fn into_flat_call_builder(self, config: &FlatCallConfig) -> ParityTraceBuilder {
        let exclusion = if config.include_precompiles.unwrap_or_default() {
            PrecompileExclusion::None
        } else {
            PrecompileExclusion::CallAndStaticCall
        };
        ParityTraceBuilder::new(self.traces.arena, exclusion)
    }

    /// Consumes the Inspector and returns a [GethTraceBuilder].
    #[inline]
    pub fn into_geth_builder(self) -> GethTraceBuilder<'static> {
        GethTraceBuilder::new(self.traces.arena)
            .with_features(self.features)
            .with_spec_id(self.spec_id.unwrap_or(SpecId::FRONTIER))
    }

    /// Returns the  [GethTraceBuilder] for the recorded traces without consuming the type.
    ///
    /// This can be useful for multiple transaction tracing (block) where this inspector can be
    /// reused for each transaction but caller must ensure that the traces are cleared before
    /// starting a new transaction: [`Self::fuse`]
    #[inline]
    pub fn geth_builder(&self) -> GethTraceBuilder<'_> {
        GethTraceBuilder::new_borrowed(self.traces.nodes())
            .with_features(self.features)
            .with_spec_id(self.spec_id.unwrap_or(SpecId::FRONTIER))
    }

    /// Returns the last trace [CallTrace] index from the stack.
    ///
    /// This will be the currently active call trace.
    ///
    /// # Panics
    ///
    /// If no [CallTrace] was pushed
    #[track_caller]
    #[inline]
    fn last_trace_idx(&self) -> usize {
        self.trace_stack.last().expect("can't start step without starting a trace first").trace_idx
    }

    /// Returns a mutable reference to the last trace [CallTrace] from the stack.
    #[track_caller]
    fn last_trace(&mut self) -> &mut CallTraceNode {
        let idx = self.last_trace_idx();
        &mut self.traces.arena[idx]
    }

    /// Removes the active call and its log-count checkpoint from the stack.
    ///
    /// # Panics
    ///
    /// If no [CallTrace] was pushed
    #[track_caller]
    #[inline]
    fn pop_trace(&mut self) -> TraceStackEntry {
        self.trace_stack.pop().expect("more traces were filled than started")
    }

    /// Starts tracking a new trace.
    ///
    /// Invoked on [Inspector::call].
    #[allow(clippy::too_many_arguments)]
    fn start_trace_on_call(
        &mut self,
        depth: usize,
        address: Address,
        input_data: Bytes,
        value: U256,
        kind: CallKind,
        caller: Address,
        gas_limit: u64,
        maybe_precompile: Option<bool>,
    ) {
        let frame_index = self.frame_transaction.as_ref().and_then(|frames| {
            (depth == self.traces.arena[0].trace.depth).then_some(frames.active)
        });
        let depth = depth + usize::from(self.frame_transaction.is_some());

        // find an empty steps vec or create a new one
        let steps = self.reusable_step_vecs.pop().unwrap_or_default();

        // the currently active call is the parent of the new call
        let parent = self
            .trace_stack
            .last()
            .map(|entry| entry.trace_idx)
            .or_else(|| self.frame_transaction.as_ref().map(|_| 0));

        let trace_idx = self.traces.push_trace(
            parent,
            CallTrace {
                frame_index,
                depth,
                address,
                kind,
                data: input_data,
                value,
                status: None,
                caller,
                maybe_precompile,
                gas_limit,
                steps,
                ..Default::default()
            },
        );
        if let Some(index) = frame_index {
            self.frame_transaction.as_mut().unwrap().nodes[index] = Some(trace_idx);
        }
        self.trace_stack.push(TraceStackEntry { trace_idx, log_count: self.log_count });
    }

    /// Fills the current trace with the outcome of a call.
    ///
    /// Invoked on [Inspector::call_end].
    ///
    /// # Panics
    ///
    /// This expects an existing trace [Self::start_trace_on_call]
    fn fill_trace_on_call_end<E>(&mut self, result: &MessageResultExt<E>) {
        let TraceStackEntry { trace_idx, log_count } = self.pop_trace();
        let trace = &mut self.traces.arena[trace_idx].trace;

        trace.gas_used = result.gas.spent();
        trace.gas_refund_counter = result.gas.refunded().max(0) as u64;

        trace.status = Some(result.stop);
        trace.success = result.is_success();
        trace.output = result.output.clone();
        if !trace.success {
            self.log_count = log_count;
        }

        // The result is the frame's outcome as the parent sees it; another inspector may have
        // overridden it, so it wins over the address recorded when the create started.
        if let Some(address) = result.created_address {
            trace.address = address;
        }
    }

    /// Starts tracking a step
    ///
    /// Invoked on [Inspector::step]
    ///
    /// # Panics
    ///
    /// This expects an existing [CallTrace], in other words, this panics if not within the context
    /// of a call.
    #[cold]
    fn start_step<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        // We always want an OpCode, even it is unknown because it could be an additional opcode
        // that not a known constant.
        let op = OpCode::new_or_unknown(interp.opcode());

        let trace_idx = self.last_trace_idx();

        let record = !self.budget.exceeded()
            && self.config.should_record_opcode(op)
            && self.config.step_limit.is_none_or(|limit| self.recorded_steps < limit.get());
        if !record {
            // Push a sentinel so that the upcoming `step_end` stays paired with this step.
            self.step_stack.push(PendingStep::new(trace_idx, usize::MAX, 0));
            return;
        }

        self.recorded_steps += 1;
        let node = &mut self.traces.arena[trace_idx];

        // Reuse the memory from the previous step if:
        // - there is not opcode filter -- in this case we cannot rely on the order of steps
        // - it exists and has not modified memory
        let memory = self
            .config
            .record_memory_snapshots
            .then(|| {
                if self.config.record_opcodes_filter.is_none()
                    && let Some(prev) = node.trace.steps.last()
                    && !prev.op.modifies_memory()
                    && let Some(memory) = &prev.memory
                {
                    return Some(memory.clone());
                }
                let bytes = interp.memory().slice(0, interp.memory().len());
                self.budget.reserve(bytes.len()).then(|| RecordedMemory::new(bytes))
            })
            .flatten();

        let stack = if self.config.record_stack_snapshots.is_all()
            || self.config.record_stack_snapshots.is_full()
        {
            Some(interp.stack().as_slice().into())
        } else if self.config.record_stack_snapshots.is_top() {
            let stack = interp.stack();
            let top = stack.as_slice().last().map(core::slice::from_ref).unwrap_or_default();
            Some(top.into())
        } else {
            None
        };
        let returndata = if self.config.record_returndata_snapshots {
            interp.return_data().clone()
        } else {
            Bytes::new()
        };

        let mut immediate_bytes = None;
        if self.config.record_immediate_bytes {
            let size = usize::from(immediate_size(op.get()));
            if size != 0 {
                let pc = interp.pc() + 1;
                let bytes = interp.original_bytecode_slice().get(pc..pc + size).unwrap_or_default();
                if self.budget.reserve(bytes.len()) {
                    immediate_bytes = Some(Bytes::copy_from_slice(bytes));
                }
            }
        }

        // A step that halts on the budget never runs, so it records no access.
        let storage_change = if self.config.record_state_diff && !self.budget.halts() {
            storage_change(interp, op.get())
        } else {
            None
        };

        let step_idx = node.trace.steps.len();
        node.trace.steps.push(CallTraceStep {
            pc: interp.pc(),
            op,
            stack,
            memory,
            returndata,
            gas_remaining: interp.gas().remaining(),
            gas_refund_counter: interp.gas().refunded() as u64,
            immediate_bytes,

            // These fields will be populated in `step_end`.
            push_stack: None,
            gas_cost: 0,
            state_gas_cost: None,
            state_gas_reservoir: interp
                .version()
                .feature(EvmFeatures::EIP8037)
                .then_some(interp.gas().reservoir()),
            state_gas_spent: interp.gas().state_gas_spent(),
            storage_change,
            status: None,

            // This is never populated in `TracingInspector`.
            decoded: None,
        });

        node.ordering.push(TraceMemberOrder::Step(step_idx));
        self.step_stack.push(PendingStep::new(
            trace_idx,
            step_idx,
            interp.gas().remaining_excluding_state_spill(),
        ));

        if self.config.record_step_deltas {
            let write_range = memory_write_range(op.get(), interp.stack().as_slice());
            let store = (!self.budget.halts())
                .then(|| storage_write(op.get(), interp.stack().as_slice()))
                .flatten();
            if write_range.is_some()
                || store.is_some()
                || node.trace.steps[step_idx].is_call_like_op()
            {
                node.trace.step_deltas.push(StepDelta {
                    step: step_idx,
                    store,
                    write_range,
                    ..Default::default()
                });
            }
        }
    }

    /// Records the gas a call or create step has charged, when its message starts.
    ///
    /// Invoked on [Inspector::call] and [Inspector::create], where the parent has charged the step,
    /// including the gas forwarded to the child, and the child has not returned any gas yet. A
    /// call step costs the forwarded gas too, a create step does not, so `forwarded` is the gas to
    /// give back.
    fn finish_step_charge<T: EvmTypes>(&mut self, interp: &Interpreter<'_, '_, T>, forwarded: u64) {
        // The step's own message is the first one it starts; a precompile may start more.
        if let Some(pending) = self.step_stack.last_mut()
            && pending.gas_after_charge.is_none()
        {
            pending.gas_after_charge =
                Some(interp.gas().remaining_excluding_state_spill().saturating_add(forwarded));
        }
    }

    /// Fills the current trace with the output of a step.
    ///
    /// Invoked on [Inspector::step_end].
    #[cold]
    fn fill_step_on_step_end<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let Some(PendingStep { trace_idx, step_idx, gas_before, gas_after_charge }) =
            self.step_stack.pop()
        else {
            return;
        };
        let node = &mut self.traces.arena[trace_idx];
        // The step is not present if it was filtered out or the capture limit was reached.
        let Some(step) = node.trace.steps.get_mut(step_idx) else {
            return;
        };

        // See comments in `start_step`.
        debug_assert!(
            step.push_stack.is_none()
                && step.gas_cost == 0
                && step.status.is_none()
                && step.decoded.is_none(),
            "step in step_end is already filled: {trace_idx} -> {step:#?}",
        );

        if self.config.record_stack_snapshots.is_all()
            || self.config.record_stack_snapshots.is_pushes()
        {
            let outputs = if step.op.is_valid() { step.op.outputs() as usize } else { 0 };
            let stack = interp.stack();
            step.push_stack = Some(
                stack
                    .as_slice()
                    .get(stack.len().saturating_sub(outputs)..)
                    .unwrap_or_default()
                    .into(),
            );
        }

        // A call or create step is charged once its message starts, which includes the gas
        // forwarded by a call and excludes it for a create. An out-of-gas step consumes the whole
        // remainder it started with, except the reentrancy sentry, which halts before any charge.
        // Any other step is charged when it ends. State gas that spills into the remaining gas is
        // not part of the cost.
        let result = interp.result();
        step.gas_cost = if result == Err(InstrStop::ReentrancySentryOOG) {
            0
        } else if let Some(after) = gas_after_charge {
            gas_before.saturating_sub(after)
        } else if result.is_err_and(InstrStop::is_out_of_gas) {
            step.gas_remaining
        } else {
            gas_before.saturating_sub(interp.gas().remaining_excluding_state_spill())
        };
        let state_gas_delta = interp.gas().state_gas_spent().saturating_sub(step.state_gas_spent);
        if step.state_gas_reservoir.is_some() && state_gas_delta != 0 {
            step.state_gas_cost = Some(state_gas_delta);
        }

        // set the status
        step.status = interp.result().err();

        if step.status.is_some_and(|status| status.is_halt()) {
            return;
        }
        // The gas a child returns, gas credits and state gas drawn from the remaining gas cannot be
        // recovered from the step cost, so record the balance after the step even without memory
        // and storage deltas.
        let remaining = interp.gas().remaining();
        let gas_remaining_after = (step.is_call_like_op()
            || remaining != step.gas_remaining.saturating_sub(step.gas_cost))
        .then_some(remaining);
        if gas_remaining_after.is_some()
            && node.trace.step_deltas.last().is_none_or(|delta| delta.step != step_idx)
        {
            node.trace.step_deltas.push(StepDelta { step: step_idx, ..Default::default() });
        }
        if let Some(delta) =
            node.trace.step_deltas.last_mut().filter(|delta| delta.step == step_idx)
        {
            delta.gas_remaining_after = gas_remaining_after;
            // Call results and returned memory are already available in evm2's `step_end`.
            if self.config.record_step_deltas {
                if step.is_call_like_op()
                    && let Some(range) = &mut delta.write_range
                {
                    range.end = range.start + range.len().min(interp.return_data().len());
                }
                delta.record_memory_write(
                    interp.memory().slice(0, interp.memory().len()),
                    &mut self.budget,
                );
            }
        }
    }
}

impl<T: EvmTypes> Inspector<T> for TracingInspector {
    fn frame_transaction_start(
        &mut self,
        tx: &Arc<TxEip8141>,
        gas: &FrameGas,
        host: &mut T::Host<'_>,
    ) {
        self.frame_transaction = Some(FrameTracing {
            payload: Arc::clone(tx),
            active: 0,
            nodes: alloc::vec![None; tx.frames.len()],
            log_start: self.log_count,
        });
        self.spec_id = Some(host.spec_id());
        self.features = host.version().features;
        self.traces.arena[0].trace = CallTrace {
            frame_transaction_root: true,
            depth: usize::from(host.version().initial_call_depth),
            address: ENTRY_POINT,
            caller: tx.sender,
            gas_limit: gas.maximum,
            ..Default::default()
        };
    }

    fn frame_start(&mut self, index: usize, _frame: &Frame, _host: &mut T::Host<'_>) {
        self.frame_transaction.as_mut().unwrap().active = index;
    }

    fn frame_end(
        &mut self,
        index: usize,
        receipt: &FrameReceipt,
        output: &Bytes,
        stop: InstrStop,
        host: &mut T::Host<'_>,
    ) {
        let frames = self.frame_transaction.as_mut().unwrap();
        frames.active = index;
        let frame = frames.payload.frames[index].clone();
        let sender = frames.payload.sender;
        let node = if let Some(node) = frames.nodes[index] {
            node
        } else {
            self.start_trace_on_call(
                usize::from(host.version().initial_call_depth),
                frame.resolved_target(sender),
                if self.config.record_inputs { frame.data.clone() } else { Bytes::new() },
                frame.value,
                if matches!(frame.mode, FrameMode::Verify | FrameMode::Assert) {
                    CallKind::StaticCall
                } else {
                    CallKind::Call
                },
                if frame.mode == FrameMode::Sender { sender } else { ENTRY_POINT },
                frame.limits.execution,
                None,
            );
            self.pop_trace().trace_idx
        };
        let trace = &mut self.traces.arena[node].trace;
        trace.frame_index = Some(index);
        trace.frame_mode = Some(frame.mode.into());
        trace.gas_limit = frame.limits.execution;
        trace.gas_used = receipt.gas_used.execution;
        trace.state_gas_used = receipt.gas_used.state;
        trace.output = output.clone();
        trace.status = Some(stop);
        trace.success = receipt.status == FrameStatus::Success;
        trace.skipped = receipt.status == FrameStatus::SkippedAtomicBatch;
    }

    fn frame_transaction_end(&mut self, result: &evm2::TxResult<T>, _host: &mut T::Host<'_>) {
        let outcome = result.frame.as_ref().expect("native frame outcome");
        let frames = self.frame_transaction.take().unwrap();
        for (index, receipt) in outcome.receipts.iter().enumerate() {
            let node = frames.nodes[index].expect("every frame has a trace node");
            let end = frames.nodes[index + 1..]
                .iter()
                .flatten()
                .next()
                .copied()
                .unwrap_or(self.traces.arena.len());
            let trace = &mut self.traces.arena[node].trace;
            trace.state_gas_used = receipt.gas_used.state;
            trace.gas_used = receipt.gas_used.execution;
            trace.output = outcome.outputs[index].clone();
            trace.success = receipt.status == FrameStatus::Success;
            trace.skipped = receipt.status == FrameStatus::SkippedAtomicBatch;
            trace.status = Some(outcome.stops[index]);
            trace.rolled_back = outcome.rolled_back[index];
            if trace.rolled_back {
                for child in &mut self.traces.arena[node..end] {
                    child.trace.rolled_back = true;
                    child.logs.clear();
                    child.ordering.retain(|member| !matches!(member, TraceMemberOrder::Log(_)));
                }
            }
        }
        // Atomic rollback can remove earlier logs after later calls have assigned their indices.
        // Reindex surviving logs in emission order, retaining the caller's block-wide offset.
        let mut visible = alloc::vec![true; self.traces.arena.len()];
        let mut logs = Vec::new();
        for node in self.traces.arena.iter().skip(1) {
            visible[node.idx] = node.trace.success
                && !node.trace.rolled_back
                && node.parent.is_none_or(|parent| visible[parent]);
            if visible[node.idx] {
                logs.extend(
                    node.logs.iter().enumerate().map(|(index, log)| (log.index, node.idx, index)),
                );
            }
        }
        logs.sort_unstable();
        for (index, (_, node, log)) in logs.into_iter().enumerate() {
            self.traces.arena[node].logs[log].index = (frames.log_start + index) as u64;
        }
        self.log_count = frames.log_start
            + outcome.receipts.iter().map(|receipt| receipt.logs.len()).sum::<usize>();
        let root = &mut self.traces.arena[0].trace;
        root.gas_used = result.tx_gas_used();
        root.state_gas_used = result.state_gas_spent;
        root.success =
            outcome.receipts.iter().all(|receipt| receipt.status == FrameStatus::Success);
        root.status = Some(result.stop);
    }

    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.last_trace().trace.entered_evm = true;
        if self.spec_id.is_none() {
            self.spec_id = Some(interp.spec());
        }
        self.features = interp.version().features;
        if self.config.record_bytecode {
            self.last_trace().trace.bytecode = Some(interp.original_bytecode());
        }
    }

    #[inline]
    fn inspector_config(&self) -> InspectorConfig {
        // The step hooks record steps, and halt the execution once a halting byte budget is
        // exceeded, which any hook can cause.
        let steps = if self.budget.can_halt() {
            OpcodeSet::ALL
        } else if self.config.record_steps {
            self.config.record_opcodes_filter.unwrap_or(OpcodeSet::ALL)
        } else {
            OpcodeSet::EMPTY
        };
        InspectorConfig::new().with_steps(steps)
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if self.config.record_steps {
            self.start_step(interp);
        }
        self.budget.halt(interp, true);
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if self.config.record_steps {
            self.fill_step_on_step_end(interp);
        }
        self.budget.halt(interp, true);
    }

    fn log(&mut self, log: &Log, _host: &mut T::Host<'_>) {
        if self.config.record_logs {
            let log_count = self.log_count;
            self.log_count += 1;
            // Embedders can emit logs outside any call, before the first or after the last. They
            // still take a log index but have no trace to attach to.
            if self.trace_stack.is_empty() {
                return;
            }
            let trace = self.last_trace();
            trace.ordering.push(TraceMemberOrder::Log(trace.logs.len()));
            trace.logs.push(
                CallLog::from(log.clone())
                    .with_position(trace.children.len() as u64)
                    .with_index(log_count as u64),
            );
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        if self.spec_id.is_none() {
            self.spec_id = Some(interp.spec());
        }
        self.features = interp.version().features;
        self.finish_step_charge(interp, 0);

        // determine correct `from` and `to` based on the call scheme
        let (from, to) = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => {
                (message.destination, message.call_target)
            }
            _ => (message.caller, message.destination),
        };

        // Builders decide whether to leave out calls to precompiles, so record which calls are.
        let maybe_precompile =
            self.config.record_precompile_calls.then(|| interp.host().precompiles().contains(&to));

        let input = if self.config.record_inputs
            && (self.trace_stack.is_empty() || self.budget.reserve(message.input.len()))
        {
            message.input.to_bytes()
        } else {
            Bytes::new()
        };
        self.start_trace_on_call(
            usize::from(message.depth),
            to,
            input,
            message.value,
            message.kind.into(),
            from,
            message.gas_limit,
            maybe_precompile,
        );

        self.budget.halt(interp, false);

        None
    }

    fn call_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.fill_trace_on_call_end(result);
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        if self.spec_id.is_none() {
            self.spec_id = Some(interp.spec());
        }
        self.features = interp.version().features;
        self.finish_step_charge(interp, message.gas_limit);

        let input = if self.config.record_inputs
            && (self.trace_stack.is_empty() || self.budget.reserve(message.input.len()))
        {
            message.input.to_bytes()
        } else {
            Bytes::new()
        };
        self.start_trace_on_call(
            usize::from(message.depth),
            message.destination,
            input,
            message.value,
            message.kind.into(),
            message.caller,
            message.gas_limit,
            Some(false),
        );
        self.budget.halt(interp, false);
        None
    }

    fn create_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.fill_trace_on_call_end(result);
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host<'_>,
    ) {
        if self.trace_stack.is_empty() {
            return;
        }
        let node = self.last_trace();
        node.trace.selfdestruct_address = Some(*contract);
        node.trace.selfdestruct_refund_target = Some(*target);
        node.trace.selfdestruct_transferred_value = Some(*value);
    }
}

/// Contains some contextual infos for a transaction execution that is made available to the JS
/// object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionContext {
    /// Hash of the block the tx is contained within.
    ///
    /// `None` if this is a call.
    pub block_hash: Option<B256>,
    /// Index of the transaction within a block.
    ///
    /// `None` if this is a call.
    pub tx_index: Option<usize>,
    /// Hash of the transaction being traced.
    ///
    /// `None` if this is a call.
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

    /// Sets the hash of the transaction.
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

/// Returns the storage access an SLOAD or SSTORE is about to make, read before the opcode runs.
fn storage_change<T: EvmTypes>(
    interp: &mut Interpreter<'_, '_, T>,
    op: u8,
) -> Option<Box<StorageChange>> {
    let (key, value, reason) = match (op, interp.stack().as_slice()) {
        (op::SLOAD, [.., key]) => (*key, None, StorageChangeReason::SLOAD),
        (op::SSTORE, [.., value, key]) => (*key, Some(*value), StorageChangeReason::SSTORE),
        _ => return None,
    };
    let address = interp.message().destination;
    // A database error leaves the record empty: an opcode that reads the slot fails on it too.
    let current = interp.host().state_mut().storage_slot_untracked(&address, &key).ok()?;
    Some(Box::new(match value {
        Some(value) => StorageChange { key, value, had_value: Some(current), reason },
        None => StorageChange { key, value: current, had_value: None, reason },
    }))
}

/// Returns the memory range whose contents after execution the opcode reports, derived from its
/// inputs on the stack.
///
/// This is the range the opcode writes, or for `MLOAD` the word it reads, as in Parity's `vmTrace`.
fn memory_write_range(op: u8, stack: &[U256]) -> Option<Range<usize>> {
    let back = |index: usize| {
        stack.get(stack.len().checked_sub(index + 1)?).and_then(|v| usize::try_from(*v).ok())
    };
    let (offset, size) = match op {
        op::MLOAD | op::MSTORE => (back(0)?, 32),
        op::MSTORE8 => (back(0)?, 1),
        op::CALLDATACOPY | op::CODECOPY | op::RETURNDATACOPY | op::MCOPY => (back(0)?, back(2)?),
        op::EXTCODECOPY => (back(1)?, back(3)?),
        op::CALL | op::CALLCODE => (back(5)?, back(6)?),
        op::DELEGATECALL | op::STATICCALL => (back(4)?, back(5)?),
        _ => return None,
    };
    (size != 0).then_some(offset..offset.checked_add(size)?)
}

/// Returns the storage write of an `SSTORE`, derived from its inputs on the stack.
///
/// This is independent of the journal, which has no entry for a warm write of the unchanged value.
const fn storage_write(op: u8, stack: &[U256]) -> Option<StorageDelta> {
    match (op, stack) {
        (op::SSTORE, [.., val, key]) => Some(StorageDelta { key: *key, val: *val }),
        _ => None,
    }
}

/// An active call and the log count to restore if it reverts.
#[derive(Clone, Debug)]
struct TraceStackEntry {
    trace_idx: usize,
    log_count: usize,
}

#[cfg(test)]
mod frame_tests {
    //! Repeated tracing releases transaction owners and retains only reusable step allocations.

    use super::*;
    use alloc::{
        sync::{Arc, Weak},
        vec,
    };
    use alloy_consensus::transaction::Recovered;
    use alloy_eips::eip8141::FrameLimits;
    use core::ops::Deref;
    use evm2::{
        BaseEvmTypes, Evm, Precompiles,
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, InMemoryDB},
    };

    const SENDER: Address = Address::with_last_byte(0xa0);
    const BODY: Address = Address::with_last_byte(0xb0);

    fn transaction(data: Bytes) -> Recovered<TxEnvelope> {
        Recovered::new_unchecked(
            TxEnvelope::Eip8141(TxEip8141 {
                chain_id: 1,
                sender: SENDER,
                nonce_keys: Some(vec![]),
                frames: [
                    (FrameMode::Verify, SENDER, 3, Bytes::new()),
                    (FrameMode::Sender, BODY, 0, data),
                ]
                .into_iter()
                .map(|(mode, target, flags, data)| Frame {
                    mode,
                    target: target.into(),
                    flags,
                    data,
                    limits: FrameLimits { execution: 100_000, state: 300_000 },
                    ..Default::default()
                })
                .collect(),
                ..Default::default()
            }),
            Address::ZERO,
        )
    }

    fn evm() -> Evm<'static, BaseEvmTypes> {
        let mut db = InMemoryDB::default();
        for (address, code) in [(SENDER, &[0x60, 3, 0x5f, 0x5f, 0xaa][..]), (BODY, &[0x00][..])] {
            db.insert_account_info(
                &address,
                AccountInfo::default()
                    .with_balance(U256::from(1_000_000_000))
                    .with_code(Bytecode::new_legacy(Bytes::copy_from_slice(code))),
            );
        }
        Evm::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            Precompiles::base(SpecId::BOGOTA),
        )
    }

    #[test]
    fn repeated_frame_and_prefix_resets_bound_the_step_buffer_pool() {
        let mut evm = evm();
        let tx = transaction(Bytes::new());
        let mut inspector = TracingInspector::new(TracingInspectorConfig::all());
        for iteration in 0..128 {
            evm.set_inspector(inspector);
            if iteration % 2 == 0 {
                evm.validate_frame_prefix(&tx, 1).unwrap();
            } else {
                assert!(evm.call_tx(&tx).unwrap().status);
            }
            inspector = *evm.clear_inspector_as::<TracingInspector>().unwrap();
            inspector.fuse();
            assert!(inspector.frame_transaction.is_none());
            assert!(inspector.trace_stack.is_empty());
            assert_eq!(inspector.traces.nodes().len(), 1);
            assert!(inspector.reusable_step_vecs.len() <= 2, "iteration {iteration}");
            assert!(inspector.reusable_step_vecs.iter().all(|steps| steps.capacity() != 0));
        }
        let pool_len = inspector.reusable_step_vecs.len();
        for _ in 0..128 {
            inspector.fuse();
            assert_eq!(inspector.reusable_step_vecs.len(), pool_len);
        }
    }

    #[derive(Debug)]
    struct OwnedPayload {
        bytes: Vec<u8>,
        _alive: Arc<()>,
    }

    impl AsRef<[u8]> for OwnedPayload {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }

    fn tracked_data() -> (Bytes, Weak<()>) {
        type InnerBytes = <Bytes as Deref>::Target;
        let alive = Arc::new(());
        let weak = Arc::downgrade(&alive);
        let data =
            Bytes(InnerBytes::from_owner(OwnedPayload { bytes: vec![42; 16_384], _alive: alive }));
        (data, weak)
    }

    #[test]
    fn native_traces_release_frame_payloads_on_completion_or_reset() {
        for failed in [false, true] {
            for prefix in [false, true] {
                let (data, weak) = tracked_data();
                let tx = transaction(data);
                let mut evm = evm();
                if failed {
                    evm.database_as_mut::<InMemoryDB>().unwrap().insert_account_info(
                        &SENDER,
                        AccountInfo::default().with_balance(U256::from(1_000_000_000)).with_code(
                            Bytecode::new_legacy(Bytes::from_static(&[0x5f, 0x5f, 0xfd])),
                        ),
                    );
                }
                evm.set_inspector(TracingInspector::new(TracingInspectorConfig::none()));
                if prefix {
                    assert_eq!(evm.validate_frame_prefix(&tx, 1).is_err(), failed);
                } else if failed {
                    assert!(evm.call_tx(&tx).is_err());
                } else {
                    assert!(evm.call_tx(&tx).unwrap().status);
                }
                let mut inspector = evm.clear_inspector_as::<TracingInspector>().unwrap();
                drop(tx);
                assert_eq!(inspector.frame_transaction.is_some(), failed);
                assert_eq!(weak.upgrade().is_some(), failed, "failed={failed}, prefix={prefix}");
                if !failed {
                    assert!(
                        inspector.traces().nodes().iter().all(|node| node.trace.data.is_empty())
                    );
                }
                inspector.fuse();
                assert!(weak.upgrade().is_none(), "failed={failed}, prefix={prefix}");
            }
        }
    }

    #[cfg(feature = "js-tracer")]
    mod js {
        use super::*;
        use crate::tracing::js::JsInspector;

        #[test]
        fn fused_js_tracers_release_frame_payload_and_callback_owners() {
            let mut evm = evm();
            let mut inspector = JsInspector::new(
                r#"{
                step: function(log) { this.log = log; },
                enter: function(frame) { this.frame = frame; },
                exit: function(result) { this.exitResult = result; },
                fault: function() {}, result: function() { return 1; }
            }"#
                .into(),
                serde_json::Value::Null,
            )
            .unwrap();
            for iteration in 0..32 {
                let (data, weak) = tracked_data();
                let tx = transaction(data);
                evm.set_inspector(inspector);
                if iteration % 2 == 0 {
                    evm.validate_frame_prefix(&tx, 1).unwrap();
                } else {
                    let result = evm.transact(&tx).unwrap().detach();
                    inspector = *evm.clear_inspector_as::<JsInspector>().unwrap();
                    let block = *evm.block();
                    assert_eq!(
                        inspector.json_result(&result, &tx, &block, evm.database_mut()).unwrap(),
                        serde_json::json!(1)
                    );
                    evm.set_inspector(inspector);
                }
                inspector = *evm.clear_inspector_as::<JsInspector>().unwrap();
                drop(tx);
                // Result collection still needs the original envelope until the inspector is reset.
                assert!(weak.upgrade().is_some());
                inspector.fuse().unwrap();
                boa_gc::force_collect();
                assert!(weak.upgrade().is_none(), "iteration {iteration}");
            }
        }
    }

    #[test]
    fn frame_start_shares_prepared_payload_and_gas() {
        let tx = Arc::new(TxEip8141::default());
        let mut evm = evm();
        let mut gas = FrameGas::calculate(&tx, evm.version()).unwrap();
        gas.maximum += 1;
        let mut inspector = TracingInspector::new(TracingInspectorConfig::none());
        <TracingInspector as Inspector<BaseEvmTypes>>::frame_transaction_start(
            &mut inspector,
            &tx,
            &gas,
            &mut evm,
        );
        assert!(Arc::ptr_eq(&inspector.frame_transaction.as_ref().unwrap().payload, &tx));
        assert_eq!(inspector.traces.nodes()[0].trace.gas_limit, gas.maximum);
        inspector.fuse();
        assert_eq!(Arc::strong_count(&tx), 1);
    }
}

/// A step waiting for `step_end`.
#[derive(Clone, Copy, Debug)]
struct PendingStep {
    trace_idx: usize,
    step_idx: usize,
    /// The remaining gas plus spilled state gas when the step started.
    gas_before: u64,
    /// The same measure once a call or create step charged everything it charges before its
    /// message starts.
    gas_after_charge: Option<u64>,
}

impl PendingStep {
    const fn new(trace_idx: usize, step_idx: usize, gas_before: u64) -> Self {
        Self { trace_idx, step_idx, gas_before, gas_after_charge: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm2::{BaseEvmTypes, Evm, Precompiles, ethereum::ethereum_tx_registry, evm::InMemoryDB};

    #[test]
    fn log_and_selfdestruct_without_active_frame() {
        let mut inspector = TracingInspector::new(TracingInspectorConfig::none().record_logs());
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            Default::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            InMemoryDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );
        let traces = inspector.traces.clone();

        Inspector::<BaseEvmTypes>::log(&mut inspector, &Log::default(), &mut host);

        assert_eq!(inspector.log_count, 1);
        assert!(inspector.traces.nodes().iter().all(|node| node.logs.is_empty()));
        assert_eq!(inspector.traces, traces);

        Inspector::<BaseEvmTypes>::selfdestruct(
            &mut inspector,
            &Address::ZERO,
            &Address::ZERO,
            &U256::ZERO,
            &mut host,
        );

        assert_eq!(inspector.traces, traces);
    }
}
