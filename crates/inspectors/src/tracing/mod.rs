//! EVM tracing inspectors.

use crate::{
    opcode::immediate_size,
    tracing::{
        arena::PushTraceKind,
        types::{
            CallKind, CallLog, CallTrace, CallTraceStep, RecordedMemory, StorageChange,
            StorageChangeReason, TraceMemberOrder,
        },
        utils::gas_used,
    },
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
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
#[derive(Clone, Debug)]
pub struct TracingInspector {
    config: TracingInspectorConfig,
    traces: CallTraceArena,
    trace_stack: Vec<usize>,
    step_stack: Vec<(usize, usize, u64, usize)>,
    last_journal_len: usize,
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
            last_journal_len: 0,
            log_index: 0,
            spec_id: None,
        }
    }

    /// Resets the inspector to its initial state.
    pub fn fuse(&mut self) {
        self.traces.clear();
        self.trace_stack.clear();
        self.step_stack.clear();
        self.last_journal_len = 0;
        self.log_index = 0;
        self.spec_id = None;
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

    const fn is_deep(&self) -> bool {
        !self.trace_stack.is_empty()
    }

    fn is_precompile_call<T: EvmTypes<Host = Evm<T>>>(
        &self,
        host: &Evm<T>,
        message: &Message<T>,
    ) -> bool {
        !message.disable_precompiles
            && self.is_deep()
            && message.value.is_zero()
            && host.precompiles().contains(&message.code_address)
    }

    fn start_trace<T: EvmTypes>(&mut self, message: &Message<T>, maybe_precompile: Option<bool>) {
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
            maybe_precompile,
            kind: message.kind.into(),
            value: message.value,
            data: message.input.clone(),
            gas_limit: message.gas_limit,
            ..Default::default()
        };

        let entry = self.trace_stack.last().copied().unwrap_or_default();
        let push_kind = if maybe_precompile.unwrap_or(false) {
            PushTraceKind::PushOnly
        } else {
            PushTraceKind::PushAndAttachToParent
        };
        let idx = self.traces.push_trace(entry, push_kind, trace);
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
                let value = host
                    .state()
                    .storage_ref(address, key)
                    .map(|slot| slot.current)
                    .unwrap_or_default();
                Some(Box::new(StorageChange {
                    key: *key,
                    value,
                    had_value: Some(*previous),
                    reason,
                }))
            }
            JournalEntry::StorageWarmed { address, key } => {
                let value = host
                    .state()
                    .storage_ref(address, key)
                    .map(|slot| slot.current)
                    .unwrap_or_default();
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
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        self.spec_id = Some(interp.spec());
        if self.trace_stack.is_empty() {
            self.start_trace(interp.message(), None);
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
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
            gas_used: gas_used(
                interp.spec(),
                interp.gas().spent(),
                interp.gas().refunded().max(0) as u64,
            ),
            gas_cost: 0,
            storage_change: None,
            status: None,
            immediate_bytes,
            decoded: None,
        };
        let step_idx = self.traces.arena[trace_idx].trace.steps.len();
        self.traces.arena[trace_idx].ordering.push(TraceMemberOrder::Step(step_idx));
        self.traces.arena[trace_idx].trace.steps.push(step);
        self.last_journal_len = host.state().journal().len();
        self.step_stack.push((trace_idx, step_idx, interp.gas().remaining(), interp.stack().len()));
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
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
            let journal = host.state().journal();
            if self.config.record_state_diff && journal.len() != self.last_journal_len {
                step.storage_change = journal
                    .get(self.last_journal_len..)
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .find_map(|entry| Self::storage_step(step, entry, host));
            }
        }
    }

    fn log(&mut self, log: &Log, _host: &mut T::Host) {
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

    fn call(&mut self, message: &mut Message<T>, host: &mut T::Host) -> Option<MessageResult<T>> {
        let maybe_precompile =
            self.config.exclude_precompile_calls.then(|| self.is_precompile_call(host, message));
        self.start_trace(message, maybe_precompile);
        None
    }

    fn call_end(
        &mut self,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
        _host: &mut T::Host,
    ) {
        self.end_trace(result);
    }

    fn create(
        &mut self,
        message: &mut Message<T>,
        _host: &mut T::Host,
    ) -> Option<MessageResult<T>> {
        self.start_trace(message, Some(false));
        None
    }

    fn create_end(
        &mut self,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
        _host: &mut T::Host,
    ) {
        self.end_trace(result);
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host,
    ) {
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
