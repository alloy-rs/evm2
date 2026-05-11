//! EVM tracing inspectors.

use alloc::{vec::Vec};
use alloy_primitives::{Address, Bytes, Log, U256};
use evm2::{
    EvmTypes, Inspector,
    bytecode::opcode::OpCode,
    interpreter::{InstrStop, Interpreter, Message, MessageKind, MessageResult},
};

mod fourbyte;
pub use fourbyte::FourByteInspector;

mod opcount;
pub use opcount::OpcodeCountInspector;

/// A recorded call trace arena.
#[derive(Clone, Debug, Default)]
pub struct CallTraceArena {
    arena: Vec<CallTraceNode>,
}

impl CallTraceArena {
    /// Returns all recorded trace nodes.
    pub fn nodes(&self) -> &[CallTraceNode] {
        &self.arena
    }

    /// Returns whether no traces were recorded.
    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    fn clear(&mut self) {
        self.arena.clear();
    }

    fn push_trace(&mut self, trace: CallTrace, parent: Option<usize>) -> usize {
        let idx = self.arena.len();
        if let Some(parent) = parent {
            self.arena[parent].children.push(idx);
        }
        self.arena.push(CallTraceNode { idx, parent, children: Vec::new(), trace });
        idx
    }
}

/// A node in the recorded call trace tree.
#[derive(Clone, Debug, Default)]
pub struct CallTraceNode {
    /// Node index in the arena.
    pub idx: usize,
    /// Parent node index.
    pub parent: Option<usize>,
    /// Child node indexes.
    pub children: Vec<usize>,
    /// Recorded trace payload.
    pub trace: CallTrace,
}

/// A recorded call or create trace.
#[derive(Clone, Debug, Default)]
pub struct CallTrace {
    /// Call kind.
    pub kind: CallKind,
    /// Call depth.
    pub depth: u16,
    /// Caller address.
    pub caller: Address,
    /// Destination or created address.
    pub address: Address,
    /// Executed code address.
    pub code_address: Address,
    /// Transferred value.
    pub value: U256,
    /// Input data.
    pub input: Bytes,
    /// Output data.
    pub output: Bytes,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas used by the frame.
    pub gas_used: u64,
    /// Whether the frame succeeded.
    pub success: bool,
    /// Stop status.
    pub status: Option<InstrStop>,
    /// Recorded steps.
    pub steps: Vec<CallTraceStep>,
    /// Recorded logs.
    pub logs: Vec<Log>,
    /// Selfdestruct source address.
    pub selfdestruct_address: Option<Address>,
    /// Selfdestruct refund target.
    pub selfdestruct_refund_target: Option<Address>,
    /// Selfdestruct transferred value.
    pub selfdestruct_transferred_value: Option<U256>,
}

/// Trace call kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CallKind {
    /// CALL.
    #[default]
    Call,
    /// STATICCALL.
    StaticCall,
    /// DELEGATECALL.
    DelegateCall,
    /// CALLCODE.
    CallCode,
    /// CREATE.
    Create,
    /// CREATE2.
    Create2,
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

/// A recorded interpreter step.
#[derive(Clone, Debug, Default)]
pub struct CallTraceStep {
    /// Program counter.
    pub pc: usize,
    /// Opcode.
    pub op: Option<OpCode>,
    /// Gas remaining before the opcode.
    pub gas_remaining: u64,
    /// Gas consumed by the opcode.
    pub gas_cost: u64,
    /// Operand stack snapshot.
    pub stack: Option<Vec<U256>>,
    /// Linear memory snapshot.
    pub memory: Option<Bytes>,
    /// Return data snapshot.
    pub returndata: Bytes,
    /// Stop status.
    pub status: Option<InstrStop>,
}

/// Stack snapshot mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StackSnapshotType {
    /// Do not record stack snapshots.
    #[default]
    None,
    /// Record the full stack.
    Full,
}

/// Opcode filter.
#[derive(Clone, Debug, Default)]
pub struct OpcodeFilter {
    enabled: Vec<u8>,
}

impl OpcodeFilter {
    /// Returns whether steps with given opcode should be traced.
    pub fn is_enabled(&self, op: OpCode) -> bool {
        self.enabled.contains(&op.get())
    }

    /// Enables tracing of given opcode.
    pub fn enable(&mut self, op: OpCode) -> &mut Self {
        if !self.is_enabled(op) {
            self.enabled.push(op.get());
        }
        self
    }

    /// Enables tracing of given opcode.
    pub fn enabled(mut self, op: OpCode) -> Self {
        self.enable(op);
        self
    }
}

/// Tracing inspector configuration.
#[derive(Clone, Debug, Default)]
pub struct TracingInspectorConfig {
    /// Whether interpreter steps are recorded.
    pub record_steps: bool,
    /// Whether emitted logs are recorded.
    pub record_logs: bool,
    /// Whether memory snapshots are recorded for steps.
    pub record_memory_snapshots: bool,
    /// Stack snapshot mode.
    pub record_stack_snapshots: StackSnapshotType,
    /// Whether return data snapshots are recorded for steps.
    pub record_returndata_snapshots: bool,
    /// Optional opcode recording filter.
    pub record_opcodes_filter: Option<OpcodeFilter>,
}

impl TracingInspectorConfig {
    /// Returns a config that records nothing.
    pub const fn none() -> Self {
        Self {
            record_steps: false,
            record_logs: false,
            record_memory_snapshots: false,
            record_stack_snapshots: StackSnapshotType::None,
            record_returndata_snapshots: false,
            record_opcodes_filter: None,
        }
    }

    /// Returns a config that records all currently supported trace data.
    pub const fn all() -> Self {
        Self {
            record_steps: true,
            record_logs: true,
            record_memory_snapshots: true,
            record_stack_snapshots: StackSnapshotType::Full,
            record_returndata_snapshots: true,
            record_opcodes_filter: None,
        }
    }

    /// Returns a geth-style default config.
    pub const fn default_geth() -> Self {
        Self::all()
    }

    /// Returns a parity-style default config.
    pub const fn default_parity() -> Self {
        Self::all()
    }

    /// Sets step recording.
    pub const fn set_steps(mut self, yes: bool) -> Self {
        self.record_steps = yes;
        self
    }

    /// Sets log recording.
    pub const fn set_record_logs(mut self, yes: bool) -> Self {
        self.record_logs = yes;
        self
    }

    /// Returns whether the opcode should be recorded.
    pub fn should_record_opcode(&self, op: OpCode) -> bool {
        self.record_opcodes_filter.as_ref().is_none_or(|filter| filter.is_enabled(op))
    }
}

/// An inspector that collects call traces.
#[derive(Clone, Debug, Default)]
pub struct TracingInspector {
    config: TracingInspectorConfig,
    traces: CallTraceArena,
    trace_stack: Vec<usize>,
    step_stack: Vec<(usize, u64)>,
}

impl TracingInspector {
    /// Returns a new instance for the given config.
    pub fn new(config: TracingInspectorConfig) -> Self {
        Self { config, ..Default::default() }
    }

    /// Resets the inspector to its initial state.
    pub fn fuse(&mut self) {
        self.traces.clear();
        self.trace_stack.clear();
        self.step_stack.clear();
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
    pub fn config_mut(&mut self) -> &mut TracingInspectorConfig {
        &mut self.config
    }

    /// Updates the config of the inspector.
    pub fn update_config(
        &mut self,
        f: impl FnOnce(TracingInspectorConfig) -> TracingInspectorConfig,
    ) {
        self.config = f(self.config.clone());
    }

    /// Gets a reference to the recorded call traces.
    pub const fn traces(&self) -> &CallTraceArena {
        &self.traces
    }

    /// Gets a mutable reference to the recorded call traces.
    pub fn traces_mut(&mut self) -> &mut CallTraceArena {
        &mut self.traces
    }

    /// Consumes the inspector and returns the recorded call traces.
    pub fn into_traces(self) -> CallTraceArena {
        self.traces
    }

    fn start_trace(&mut self, message: &Message) {
        let parent = self.trace_stack.last().copied();
        let trace = CallTrace {
            kind: message.kind.into(),
            depth: message.depth,
            caller: message.caller,
            address: message.destination,
            code_address: message.code_address,
            value: message.value,
            input: message.input.clone(),
            gas_limit: message.gas_limit,
            ..Default::default()
        };
        let idx = self.traces.push_trace(trace, parent);
        self.trace_stack.push(idx);
    }

    fn end_trace(&mut self, result: &MessageResult) {
        let Some(idx) = self.trace_stack.pop() else {
            return;
        };
        let trace = &mut self.traces.arena[idx].trace;
        trace.status = Some(result.stop);
        trace.success = result.stop.is_success();
        trace.output = result.output.clone();
        trace.gas_used = result.gas.spent();
        if let Some(address) = result.created_address {
            trace.address = address;
        }
    }
}

impl<T: EvmTypes> Inspector<T> for TracingInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>) {
        if self.trace_stack.is_empty() {
            self.start_trace(interp.message());
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        if !self.config.record_steps {
            return;
        }
        let Some(trace_idx) = self.trace_stack.last().copied() else {
            return;
        };
        let Some(op) = OpCode::new(interp.opcode()) else {
            return;
        };
        if !self.config.should_record_opcode(op) {
            return;
        }
        let stack = (self.config.record_stack_snapshots == StackSnapshotType::Full)
            .then(|| interp.stack().to_vec());
        let memory = self
            .config
            .record_memory_snapshots
            .then(|| Bytes::copy_from_slice(interp.memory_ref().slice(0, interp.memory_ref().len())));
        let returndata =
            if self.config.record_returndata_snapshots { interp.return_data().clone() } else { Bytes::new() };
        let step = CallTraceStep {
            pc: interp.pc(),
            op: Some(op),
            gas_remaining: interp.gas().remaining(),
            stack,
            memory,
            returndata,
            ..Default::default()
        };
        let step_idx = self.traces.arena[trace_idx].trace.steps.len();
        self.traces.arena[trace_idx].trace.steps.push(step);
        self.step_stack.push((step_idx, interp.gas().remaining()));
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        if !self.config.record_steps {
            return;
        }
        let Some(trace_idx) = self.trace_stack.last().copied() else {
            return;
        };
        let Some((step_idx, gas_remaining)) = self.step_stack.pop() else {
            return;
        };
        if let Some(step) = self.traces.arena[trace_idx].trace.steps.get_mut(step_idx) {
            step.gas_cost = gas_remaining.saturating_sub(interp.gas().remaining());
        }
    }

    fn log(&mut self, log: &Log) {
        if !self.config.record_logs {
            return;
        }
        if let Some(trace_idx) = self.trace_stack.last().copied() {
            self.traces.arena[trace_idx].trace.logs.push(log.clone());
        }
    }

    fn call(&mut self, message: &mut Message) -> Option<MessageResult> {
        self.start_trace(message);
        None
    }

    fn call_end(&mut self, _message: &Message, result: &mut MessageResult) {
        self.end_trace(result);
    }

    fn create(&mut self, message: &mut Message) -> Option<MessageResult> {
        self.start_trace(message);
        None
    }

    fn create_end(&mut self, _message: &Message, result: &mut MessageResult) {
        self.end_trace(result);
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        if let Some(trace_idx) = self.trace_stack.last().copied() {
            let trace = &mut self.traces.arena[trace_idx].trace;
            trace.selfdestruct_address = Some(contract);
            trace.selfdestruct_refund_target = Some(target);
            trace.selfdestruct_transferred_value = Some(value);
        }
    }
}

/// A minimal geth trace builder over recorded call traces.
#[derive(Clone, Debug)]
pub struct GethTraceBuilder<'a> {
    nodes: &'a [CallTraceNode],
}

impl<'a> GethTraceBuilder<'a> {
    /// Creates a borrowed builder.
    pub const fn new_borrowed(nodes: &'a [CallTraceNode]) -> Self {
        Self { nodes }
    }

    /// Returns the recorded nodes.
    pub const fn nodes(&self) -> &[CallTraceNode] {
        self.nodes
    }
}

/// A minimal parity trace builder over recorded call traces.
#[derive(Clone, Debug)]
pub struct ParityTraceBuilder {
    nodes: Vec<CallTraceNode>,
}

impl ParityTraceBuilder {
    /// Creates a builder.
    pub fn new(nodes: Vec<CallTraceNode>) -> Self {
        Self { nodes }
    }

    /// Returns the recorded nodes.
    pub fn nodes(&self) -> &[CallTraceNode] {
        &self.nodes
    }
}
