//! EVM tracing inspectors.

use crate::{
    opcode::immediate_size,
    tracing::{
        arena::PushTraceKind,
        types::{
            CallKind, CallTraceNode, RecordedMemory, StorageChange, StorageChangeReason,
            TraceMemberOrder,
        },
        utils::gas_used,
    },
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use core::mem;
use evm2::{
    Evm, EvmTypes, Inspector, SpecId,
    bytecode::opcode::{OpCode, op},
    evm::JournalEntry,
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
pub use debug::{DebugInspector, DebugInspectorError, NoopInspector};

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
    /// Records all call traces
    traces: CallTraceArena,
    /// Tracks active calls
    trace_stack: Vec<usize>,
    /// Tracks recorded steps waiting for `step_end`.
    ///
    /// This is a stack because nested calls are executed between the `step` and `step_end` of the
    /// call instruction itself. A `usize::MAX` step index marks a step that was not recorded.
    step_stack: Vec<(usize, usize)>,
    /// Tracks the journal len in the step, used in step_end to check if the journal has changed
    last_journal_len: usize,
    /// The spec id of the EVM.
    ///
    /// This is filled during execution.
    spec_id: Option<SpecId>,
    /// Pool of reusable _empty_ step vectors to reduce allocations.
    ///
    /// All `Vec<CallTraceStep>` are always empty but may have capacity.
    reusable_step_vecs: Vec<Vec<CallTraceStep>>,
}

impl TracingInspector {
    /// Returns a new instance for the given config
    pub fn new(config: TracingInspectorConfig) -> Self {
        Self { config, ..Default::default() }
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
            step_stack,
            last_journal_len,
            spec_id,
            // kept
            config,
            reusable_step_vecs,
        } = self;

        // if we record steps we can reuse the individual calltracestep vecs
        if config.record_steps {
            for node in &mut traces.arena {
                // move out and store the reusable steps vec
                let mut steps = mem::take(&mut node.trace.steps);
                // ensure steps are cleared
                steps.clear();
                reusable_step_vecs.push(steps);
            }
        }

        traces.clear();
        trace_stack.clear();
        step_stack.clear();
        spec_id.take();
        *last_journal_len = 0;
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
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.gas_limit = gas_limit;
        }
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
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.caller = caller;
        }
    }

    /// Consumes the Inspector and returns a [ParityTraceBuilder].
    #[inline]
    pub fn into_parity_builder(self) -> ParityTraceBuilder {
        ParityTraceBuilder::new(self.traces.arena, self.spec_id, self.config)
    }

    /// Consumes the Inspector and returns a [GethTraceBuilder].
    #[inline]
    pub fn into_geth_builder(self) -> GethTraceBuilder<'static> {
        GethTraceBuilder::new(self.traces.arena)
    }

    /// Returns the  [GethTraceBuilder] for the recorded traces without consuming the type.
    ///
    /// This can be useful for multiple transaction tracing (block) where this inspector can be
    /// reused for each transaction but caller must ensure that the traces are cleared before
    /// starting a new transaction: [`Self::fuse`]
    #[inline]
    pub fn geth_builder(&self) -> GethTraceBuilder<'_> {
        GethTraceBuilder::new_borrowed(&self.traces.arena)
    }

    /// Returns true if we're no longer in the context of the root call.
    const fn is_deep(&self) -> bool {
        // the root call will always be the first entry in the trace stack
        !self.trace_stack.is_empty()
    }

    /// Returns how many logs we already recorded.
    fn log_count(&self) -> usize {
        self.traces.nodes().iter().map(|trace| trace.log_count()).sum()
    }

    /// Returns true if this a call to a precompile contract.
    ///
    /// Returns true if the `to` address is a precompile contract and the value is zero.
    #[inline]
    fn is_precompile_call<T: EvmTypes<Host = Evm<T>>>(
        &self,
        host: &Evm<T>,
        to: &Address,
        value: &U256,
    ) -> bool {
        if host.precompiles().contains(to) {
            // only if this is _not_ the root call
            return self.is_deep() && value.is_zero();
        }
        false
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
        self.trace_stack.last().copied().expect("can't start step without starting a trace first")
    }

    /// Returns a mutable reference to the last trace [CallTrace] from the stack.
    #[track_caller]
    fn last_trace(&mut self) -> &mut CallTraceNode {
        let idx = self.last_trace_idx();
        &mut self.traces.arena[idx]
    }

    /// _Removes_ the last trace [CallTrace] index from the stack.
    ///
    /// # Panics
    ///
    /// If no [CallTrace] was pushed
    #[track_caller]
    #[inline]
    fn pop_trace_idx(&mut self) -> usize {
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
        // This will only be true if the inspector is configured to exclude precompiles and the call
        // is to a precompile
        let push_kind = if maybe_precompile.unwrap_or(false) {
            // We don't want to track precompiles
            PushTraceKind::PushOnly
        } else {
            PushTraceKind::PushAndAttachToParent
        };

        // find an empty steps vec or create a new one
        let steps = self.reusable_step_vecs.pop().unwrap_or_default();

        self.trace_stack.push(self.traces.push_trace(
            0,
            push_kind,
            CallTrace {
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
        ));
    }

    /// Fills the current trace with the outcome of a call.
    ///
    /// Invoked on [Inspector::call_end].
    ///
    /// # Panics
    ///
    /// This expects an existing trace [Self::start_trace_on_call]
    fn fill_trace_on_call_end<T: EvmTypes>(&mut self, result: &MessageResult<T>) {
        let trace_idx = self.pop_trace_idx();
        let trace = &mut self.traces.arena[trace_idx].trace;

        trace.gas_used = result.gas.spent();
        trace.gas_refund_counter = result.gas.refunded().max(0) as u64;

        trace.status = Some(result.stop);
        trace.success = result.stop.is_success();
        trace.output = result.output.clone();

        if let Some(address) = result.created_address {
            // A new contract was created via CREATE
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
    fn start_step<T: EvmTypes<Host = Evm<T>>>(&mut self, interp: &mut Interpreter<'_, T>) {
        // We always want an OpCode, even it is unknown because it could be an additional opcode
        // that not a known constant.
        let op = OpCode::new_or_unknown(interp.opcode());

        let trace_idx = self.last_trace_idx();

        let record = self.config.should_record_opcode(op);
        if !record {
            // Push a sentinel so that the upcoming `step_end` stays paired with this step.
            self.step_stack.push((trace_idx, usize::MAX));
            return;
        }

        let node = &mut self.traces.arena[trace_idx];

        // Reuse the memory from the previous step if:
        // - there is not opcode filter -- in this case we cannot rely on the order of steps
        // - it exists and has not modified memory
        let memory = self.config.record_memory_snapshots.then(|| {
            if self.config.record_opcodes_filter.is_none()
                && let Some(prev) = node.trace.steps.last()
                && !prev.op.modifies_memory()
                && let Some(memory) = &prev.memory
            {
                return memory.clone();
            }
            RecordedMemory::new(interp.memory_ref().slice(0, interp.memory_ref().len()))
        });

        let stack = if self.config.record_stack_snapshots.is_all()
            || self.config.record_stack_snapshots.is_full()
        {
            Some(interp.stack().as_slice().into())
        } else {
            None
        };
        let returndata = if self.config.record_returndata_snapshots {
            interp.return_data().clone()
        } else {
            Bytes::new()
        };

        let gas_used =
            gas_used(interp.spec(), interp.gas().spent(), interp.gas().refunded() as u64);

        let mut immediate_bytes = None;
        if self.config.record_immediate_bytes {
            let size = usize::from(immediate_size(op.get()));
            if size != 0 {
                let pc = interp.pc() + 1;
                let bytes = interp.bytecode().as_slice().get(pc..pc + size).unwrap_or_default();
                immediate_bytes = Some(Bytes::copy_from_slice(bytes));
            }
        }

        self.last_journal_len = interp.host().state().journal().len();

        let step_idx = node.trace.steps.len();
        node.trace.steps.push(CallTraceStep {
            pc: interp.pc(),
            op,
            stack,
            memory,
            returndata,
            gas_remaining: interp.gas().remaining(),
            gas_refund_counter: interp.gas().refunded() as u64,
            gas_used,
            immediate_bytes,

            // These fields will be populated in `step_end`.
            push_stack: None,
            gas_cost: 0,
            storage_change: None,
            status: None,

            // This is never populated in `TracingInspector`.
            decoded: None,
        });

        node.ordering.push(TraceMemberOrder::Step(step_idx));
        self.step_stack.push((trace_idx, step_idx));
    }

    /// Fills the current trace with the output of a step.
    ///
    /// Invoked on [Inspector::step_end].
    #[cold]
    fn fill_step_on_step_end<T: EvmTypes<Host = Evm<T>>>(
        &mut self,
        interp: &mut Interpreter<'_, T>,
    ) {
        let Some((trace_idx, step_idx)) = self.step_stack.pop() else {
            return;
        };
        let node = &mut self.traces.arena[trace_idx];
        // The step is not present if it was filtered out by the opcode filter.
        let Some(step) = node.trace.steps.get_mut(step_idx) else {
            return;
        };

        // See comments in `start_step`.
        debug_assert!(
            step.push_stack.is_none()
                && step.gas_cost == 0
                && step.storage_change.is_none()
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

        let host = interp.host();
        let journal = host.state().journal();

        // If journal has not changed, there is no state change to be recorded.
        if self.config.record_state_diff && journal.len() != self.last_journal_len {
            step.storage_change = journal
                .get(self.last_journal_len..)
                .unwrap_or_default()
                .iter()
                .rev()
                .find_map(|entry| Self::storage_step(step, entry, host));
        }

        // The gas cost is the difference between the recorded gas remaining at the start of the
        // step the remaining gas here, at the end of the step.
        // TODO: Figure out why this can overflow. https://github.com/paradigmxyz/revm-inspectors/pull/38
        step.gas_cost = step.gas_remaining.saturating_sub(interp.gas().remaining());

        // set the status
        step.status = interp.result().err();
    }

    fn storage_step<T: EvmTypes<Host = Evm<T>>>(
        step: &mut CallTraceStep,
        journal_entry: &JournalEntry,
        host: &Evm<T>,
    ) -> Option<Box<StorageChange>> {
        let reason = (match step.op.get() {
            op::SLOAD => Some(StorageChangeReason::SLOAD),
            op::SSTORE => Some(StorageChangeReason::SSTORE),
            _ => None,
        })?;

        match journal_entry {
            JournalEntry::StorageChange { address, key, previous } => {
                // SAFETY: (Address, key) exists if part of StorageChange.
                let value = host.state().storage_ref(address, key).unwrap_or_default();
                Some(Box::new(StorageChange {
                    key: *key,
                    value,
                    had_value: Some(*previous),
                    reason,
                }))
            }
            JournalEntry::StorageInserted { address, key } => {
                let slot = host.state().storage_tracked_ref(address, key)?;
                Some(Box::new(StorageChange {
                    key: *key,
                    value: slot.current,
                    had_value: Some(slot.original),
                    reason,
                }))
            }
            JournalEntry::StorageWarmed { address, key } => {
                // SAFETY: (Address, key) exists if part of StorageChange.
                let value = host.state().storage_ref(address, key).unwrap_or_default();
                Some(Box::new(StorageChange { key: *key, value, had_value: None, reason }))
            }
            _ => None,
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

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for TracingInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        self.spec_id = Some(interp.spec());
        if self.trace_stack.is_empty() {
            let message = interp.message();
            self.start_trace_on_call(
                usize::from(message.depth),
                message.destination,
                message.input.clone(),
                message.value,
                message.kind.into(),
                message.caller,
                message.gas_limit,
                None,
            );
        }
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        if self.config.record_steps {
            self.start_step(interp);
        }
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        if self.config.record_steps {
            self.fill_step_on_step_end(interp);
        }
    }

    fn log(&mut self, log: &Log, _host: &mut T::Host) {
        if self.config.record_logs {
            // index starts at 0
            let log_count = self.log_count();
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
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        // determine correct `from` and `to` based on the call scheme
        let (from, to) = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => {
                (message.destination, message.code_address)
            }
            _ => (message.caller, message.destination),
        };

        let value = message.value;

        // if calls to precompiles should be excluded, check whether this is a call to a precompile
        let maybe_precompile = self.config.exclude_precompile_calls.then(|| {
            !message.disable_precompiles && self.is_precompile_call(interp.host(), &to, &value)
        });

        self.start_trace_on_call(
            usize::from(message.depth),
            to,
            message.input.clone(),
            value,
            message.kind.into(),
            from,
            message.gas_limit,
            maybe_precompile,
        );

        None
    }

    fn call_end(
        &mut self,
        _interp: &mut Interpreter<'_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.fill_trace_on_call_end(result);
    }

    fn create(
        &mut self,
        _interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.start_trace_on_call(
            usize::from(message.depth),
            message.destination,
            message.input.clone(),
            message.value,
            message.kind.into(),
            message.caller,
            message.gas_limit,
            Some(false),
        );
        None
    }

    fn create_end(
        &mut self,
        _interp: &mut Interpreter<'_, T>,
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
        _host: &mut T::Host,
    ) {
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
