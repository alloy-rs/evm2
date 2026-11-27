//! Types for representing call trace items.

use crate::tracing::{config::TraceStyle, utils, utils::convert_memory};
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
pub use alloy_primitives::Log;
use alloy_primitives::{Address, Bytes, FixedBytes, LogData, U256};
use alloy_rpc_types_trace::{
    geth::{CallFrame, CallLogFrame, GethDefaultTracingOptions, StructLog},
    parity::{
        Action, ActionType, CallAction, CallOutput, CallType, CreateAction, CreateOutput,
        CreationMethod, MemoryDelta, SelfdestructAction, StorageDelta, TraceOutput,
        TransactionTrace,
    },
};
use core::ops::Range;
use evm2::interpreter::{
    InstrStop, MessageKind,
    opcode::{OpCode, op},
};

/// Decoded call data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DecodedCallData {
    /// The function signature.
    pub signature: String,
    /// The function arguments.
    pub args: Vec<String>,
}

/// Additional decoded data enhancing the [CallTrace].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DecodedCallTrace {
    /// Optional decoded label for the call.
    pub label: Option<String>,
    /// Optional decoded return data.
    pub return_data: Option<String>,
    /// Optional decoded call data.
    pub call_data: Option<DecodedCallData>,
}

/// A trace of a call with optional decoded data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize), serde(from = "trace_serde::CallTraceDe"))]
pub struct CallTrace {
    /// Synthetic root collecting the ordered frames of an EIP-8141 transaction.
    pub frame_transaction_root: bool,
    /// Whether this call initialized an interpreter, rather than stopping at entry.
    pub entered_evm: bool,
    /// Ordered frame index for top-level frame transaction nodes.
    pub frame_index: Option<usize>,
    /// Protocol frame mode, encoded as 0..=3.
    pub frame_mode: Option<u8>,
    /// Final state gas attributed after cross-frame restoration and rollback.
    pub state_gas_used: u64,
    /// Whether the frame belongs to a rolled-back atomic group or assertion body.
    pub rolled_back: bool,
    /// Whether this frame was skipped by atomic/assertion control flow.
    pub skipped: bool,

    /// The inspector-visible depth of the call.
    ///
    /// Top-level calls start at [`evm2::Evm::initial_call_depth`], normally zero. Above zero,
    /// they are children of an empty root at depth zero.
    ///
    /// The depth of a call's steps is the depth of the call plus one.
    pub depth: u16,
    /// Whether the call was successful.
    pub success: bool,
    /// The caller address.
    pub caller: Address,
    /// The target address of this call.
    ///
    /// This is:
    /// - [`CallKind::Call`] and alike: the callee, the address of the contract being called
    /// - [`CallKind::Create`] and alike: the address of the created contract
    pub address: Address,
    /// Whether this is a call to a precompile.
    ///
    /// Only recorded if
    /// [`TracingInspectorConfig::record_precompile_calls`](crate::tracing::TracingInspectorConfig::record_precompile_calls)
    /// is set, otherwise `None`.
    pub maybe_precompile: Option<bool>,
    /// The selfdestruct this call executed, `None` for the calls that executed none.
    pub selfdestruct: Option<Box<SelfdestructTrace>>,
    /// The kind of call.
    pub kind: CallKind,
    /// The value transferred in the call.
    pub value: U256,
    /// The calldata/input, or the init code for contract creations.
    ///
    /// Empty when
    /// [`TracingInspectorConfig::record_inputs`](crate::tracing::TracingInspectorConfig::record_inputs)
    /// is disabled.
    pub data: Bytes,
    /// The return data, or the runtime bytecode of the created contract.
    pub output: Bytes,
    /// The bytecode executed by this frame, including initcode and resolved delegation code.
    ///
    /// `None` if recording is disabled or the frame never initializes an interpreter.
    /// `Some` contains the recorded bytes, which may be empty.
    /// See [`TracingInspectorConfig::record_bytecode`](crate::tracing::TracingInspectorConfig::record_bytecode).
    pub bytecode: Option<Bytes>,
    /// The total gas cost of the call.
    pub gas_used: u64,
    /// The gas limit of the call.
    pub gas_limit: u64,
    /// The refund counter of this call's frame at its end.
    pub gas_refund_counter: u64,
    /// The final status of the call.
    pub status: Option<InstrStop>,
    /// Opcode-level execution steps.
    pub steps: Vec<CallTraceStep>,
    /// The deltas recorded for [`Self::steps`], in step order.
    ///
    /// Only steps that report memory or storage, make a call or gain gas have an entry. Memory
    /// and storage require [`record_step_deltas`]; gas balances are recorded independently.
    ///
    /// [`record_step_deltas`]: crate::tracing::TracingInspectorConfig::record_step_deltas
    pub step_deltas: Vec<StepDelta>,
    /// Optional complementary decoded call data.
    pub decoded: Option<Box<DecodedCallTrace>>,
}

impl CallTrace {
    /// Returns whether execution or the protocol frame failed.
    #[inline]
    pub const fn is_error(&self) -> bool {
        if self.skipped || (self.frame_transaction_root && !self.success) {
            return true;
        }
        let Some(status) = self.status else {
            return false;
        };
        !status.is_success()
    }

    /// Returns true if the status code is a revert.
    #[inline]
    pub fn is_revert(&self) -> bool {
        self.status.is_some_and(InstrStop::is_revert)
    }

    /// Returns `true` if this trace was a selfdestruct.
    ///
    /// See also `TracingInspector::selfdestruct`.
    ///
    /// We can't rely entirely on [`Self::status`] being [`InstrStop::SelfDestruct`]
    /// because there's an edge case where a new created contract (CREATE) is immediately
    /// selfdestructed.
    ///
    /// We also can't rely entirely on `selfdestruct_refund_target` being `Some` as the
    /// `selfdestruct` inspector function will not be called after the Cancun hardfork.
    #[inline]
    pub const fn is_selfdestruct(&self) -> bool {
        matches!(self.status, Some(InstrStop::SelfDestruct))
            || self.selfdestruct_refund_target().is_some()
    }

    /// Returns the address of the selfdestructed contract.
    #[inline]
    pub const fn selfdestruct_address(&self) -> Option<Address> {
        match &self.selfdestruct {
            Some(selfdestruct) => selfdestruct.address,
            None => None,
        }
    }

    /// Returns the selfdestruct refund target.
    ///
    /// This is only `Some` if a selfdestruct was executed and the call is executed before the
    /// Cancun hardfork.
    ///
    /// See [`is_selfdestruct`](Self::is_selfdestruct) for more information.
    #[inline]
    pub const fn selfdestruct_refund_target(&self) -> Option<Address> {
        match &self.selfdestruct {
            Some(selfdestruct) => selfdestruct.refund_target,
            None => None,
        }
    }

    /// Returns the value transferred on a selfdestruct.
    ///
    /// This is only `Some` if a selfdestruct was executed and the call is executed before the
    /// Cancun hardfork.
    ///
    /// See [`is_selfdestruct`](Self::is_selfdestruct) for more information.
    #[inline]
    pub const fn selfdestruct_transferred_value(&self) -> Option<U256> {
        match &self.selfdestruct {
            Some(selfdestruct) => selfdestruct.transferred_value,
            None => None,
        }
    }

    /// Returns the error message if it is an erroneous result.
    pub(crate) fn as_error_msg(&self, kind: TraceStyle) -> Option<String> {
        if self.frame_transaction_root && !self.success {
            return Some("frame transaction failed".to_string());
        }
        if self.skipped {
            return Some("frame skipped".to_string());
        }
        self.status.and_then(|status| utils::fmt_error_msg(status, kind))
    }

    /// Gets the decoded call trace.
    ///
    /// Initializes with the default value if not yet set.
    pub fn decoded(&mut self) -> &mut DecodedCallTrace {
        self.decoded.get_or_insert_with(Default::default)
    }

    #[allow(dead_code)]
    pub(crate) fn decoded_label<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.decoded.as_ref().and_then(|d| d.label.as_deref()).unwrap_or(fallback)
    }

    #[allow(dead_code)]
    pub(crate) fn decoded_call_data(&self) -> Option<&DecodedCallData> {
        self.decoded.as_ref()?.call_data.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn decoded_return_data(&self) -> Option<&str> {
        self.decoded.as_ref()?.return_data.as_deref()
    }
}

/// The selfdestruct a call executed, see [`CallTrace::selfdestruct`].
///
/// Boxed in the [`CallTrace`], since most calls execute no selfdestruct.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelfdestructTrace {
    /// The address of the selfdestructed contract.
    pub address: Option<Address>,
    /// The selfdestruct refund target.
    pub refund_target: Option<Address>,
    /// The value transferred on the selfdestruct.
    pub transferred_value: Option<U256>,
}

/// Additional decoded data enhancing the [CallLog].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DecodedCallLog {
    /// The decoded event name.
    pub name: Option<String>,
    /// The decoded log parameters, a vector of the parameter name (e.g. foo) and the parameter
    /// value (e.g. 0x9d3...45ca).
    pub params: Option<Vec<(String, String)>>,
}

/// A log with optional decoded data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallLog {
    /// The address of the log emitter.
    pub address: Address,
    /// The raw log data.
    pub raw_log: LogData,
    /// Optional complementary decoded log data.
    pub decoded: Option<Box<DecodedCallLog>>,
    /// The position of the log relative to subcalls within the same trace.
    pub position: u64,
    /// The position of the log relative to subcalls within the same trace.
    pub index: u64,
}

impl From<Log> for CallLog {
    /// Converts a [`Log`] into a [`CallLog`].
    fn from(log: Log) -> Self {
        Self { address: log.address, raw_log: log.data, decoded: None, position: 0, index: 0 }
    }
}

impl CallLog {
    /// Sets the position of the log.
    #[inline]
    pub const fn with_position(mut self, position: u64) -> Self {
        self.position = position;
        self
    }

    /// Sets index of the log in the transaction.
    #[inline]
    pub const fn with_index(mut self, index: u64) -> Self {
        self.index = index;
        self
    }

    /// Gets the decoded call log.
    ///
    /// Initializes with the default value if not yet set.
    pub fn decoded(&mut self) -> &mut DecodedCallLog {
        self.decoded.get_or_insert_with(Default::default)
    }

    #[allow(dead_code)]
    pub(crate) fn decoded_name(&self) -> Option<&str> {
        self.decoded.as_deref()?.name.as_deref()
    }

    #[allow(dead_code)]
    pub(crate) fn decoded_params(&self) -> Option<&[(String, String)]> {
        self.decoded.as_deref()?.params.as_deref()
    }
}

/// A node in the arena
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallTraceNode {
    /// Parent node index in the arena
    pub parent: Option<usize>,
    /// Children node indexes in the arena
    pub children: Vec<usize>,
    /// This node's index in the arena
    pub idx: usize,
    /// The call trace
    pub trace: CallTrace,
    /// Recorded logs, if enabled
    pub logs: Vec<CallLog>,
    /// Ordering of child calls and logs
    pub ordering: Vec<TraceMemberOrder>,
    /// The message destination when it differs from the execution address implied by the trace.
    /// An inspector can redirect a message on entry or override the returned CREATE address.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
    pub execution_address_override: Option<Address>,
}

impl CallTraceNode {
    /// Returns the call context's execution address
    ///
    /// See `Inspector::call` impl of [TracingInspector](crate::tracing::TracingInspector)
    pub const fn execution_address(&self) -> Address {
        if let Some(address) = self.execution_address_override {
            address
        } else if self.trace.kind.is_delegate() {
            self.trace.caller
        } else {
            self.trace.address
        }
    }

    /// Iterates over steps in execution order, pairing each step with the child it called.
    ///
    /// A child is recorded right after the step that called it, so the node's `ordering` locates
    /// it. A call opcode without a following child either failed before entering a frame, e.g. due
    /// to out of gas (<https://github.com/paradigmxyz/reth/issues/3915>), or called an excluded
    /// precompile that was not attached to this node.
    pub(crate) fn steps_with_children(&self) -> impl Iterator<Item = CallTraceStepStackItem<'_>> {
        let mut ordering = self.ordering.iter().peekable();
        self.trace.steps.iter().enumerate().map(move |(step_idx, step)| {
            ordering
                .find(|member| matches!(member, TraceMemberOrder::Step(idx) if *idx == step_idx));
            let call_child_id = match ordering.next_if(|member| {
                step.is_call_like_op() && matches!(member, TraceMemberOrder::Call(_))
            }) {
                Some(TraceMemberOrder::Call(child)) => self.children.get(*child).copied(),
                _ => None,
            };
            CallTraceStepStackItem { trace_node: self, step, call_child_id }
        })
    }

    /// Returns true if this is a call to a precompile
    #[inline]
    pub fn is_precompile(&self) -> bool {
        self.trace.maybe_precompile.unwrap_or(false) && self.trace.frame_index.is_none()
    }

    /// Returns the kind of call the trace belongs to
    #[inline]
    pub const fn kind(&self) -> CallKind {
        self.trace.kind
    }

    /// Returns the status of the call
    #[inline]
    pub const fn status(&self) -> Option<InstrStop> {
        self.trace.status
    }

    /// Returns the call context's 4 byte selector
    pub fn selector(&self) -> Option<FixedBytes<4>> {
        self.trace.data.first_chunk().map(|chunk| FixedBytes::new(*chunk))
    }

    /// Returns `true` if this trace was a selfdestruct.
    ///
    /// See [`CallTrace::is_selfdestruct`] for more details.
    #[inline]
    pub const fn is_selfdestruct(&self) -> bool {
        self.trace.is_selfdestruct()
    }

    /// Converts this node into a parity `TransactionTrace`
    pub fn parity_transaction_trace(&self, trace_address: Vec<usize>) -> TransactionTrace {
        let action = self.parity_action();
        let result = if self.trace.is_error()
            && !self.trace.is_revert()
            && !self.trace.frame_transaction_root
        {
            // if the trace is a selfdestruct or an error that is not a revert, the result is None
            None
        } else {
            Some(self.parity_trace_output())
        };
        let error = self.trace.as_error_msg(TraceStyle::Parity);
        TransactionTrace { action, error, result, trace_address, subtraces: self.children.len() }
    }

    /// Returns the `Output` for a parity trace.
    ///
    /// A reverted creation reports its gas and revert data like a call, since no contract exists
    /// at the address it would have created.
    pub fn parity_trace_output(&self) -> TraceOutput {
        if self.kind().is_any_create() && !self.trace.is_revert() {
            TraceOutput::Create(CreateOutput {
                gas_used: self.trace.gas_used,
                code: self.trace.output.clone(),
                address: self.trace.address,
            })
        } else {
            TraceOutput::Call(CallOutput {
                gas_used: self.trace.gas_used,
                output: self.trace.output.clone(),
            })
        }
    }

    /// If the trace is a selfdestruct, returns the `Action` for a parity trace.
    pub fn parity_selfdestruct_action(&self) -> Option<Action> {
        self.is_selfdestruct().then(|| {
            Action::Selfdestruct(SelfdestructAction {
                address: self.trace.selfdestruct_address().unwrap_or_default(),
                refund_address: self.trace.selfdestruct_refund_target().unwrap_or_default(),
                balance: self.trace.selfdestruct_transferred_value().unwrap_or_default(),
            })
        })
    }

    /// If the trace is a selfdestruct, returns the `CallFrame` for a geth call trace
    pub fn geth_selfdestruct_call_trace(&self) -> Option<CallFrame> {
        self.is_selfdestruct().then(|| CallFrame {
            typ: "SELFDESTRUCT".to_string(),
            from: self.trace.selfdestruct_address().unwrap_or_default(),
            to: self.trace.selfdestruct_refund_target(),
            value: self.trace.selfdestruct_transferred_value(),
            ..Default::default()
        })
    }

    /// If the trace is a selfdestruct, returns the `TransactionTrace` for a parity trace.
    pub fn parity_selfdestruct_trace(&self, trace_address: Vec<usize>) -> Option<TransactionTrace> {
        let trace = self.parity_selfdestruct_action()?;
        Some(TransactionTrace {
            action: trace,
            error: None,
            result: None,
            trace_address,
            subtraces: 0,
        })
    }

    /// Returns the `Action` for a parity trace.
    ///
    /// Caution: This does not include the selfdestruct action, if the trace is a selfdestruct,
    /// since those are handled in addition to the call action.
    pub fn parity_action(&self) -> Action {
        match self.kind() {
            CallKind::Call
            | CallKind::StaticCall
            | CallKind::CallCode
            | CallKind::DelegateCall
            | CallKind::AuthCall => Action::Call(CallAction {
                from: self.trace.caller,
                to: self.trace.address,
                value: self.trace.value,
                gas: self.trace.gas_limit,
                input: self.trace.data.clone(),
                call_type: self.kind().into(),
            }),
            CallKind::Create | CallKind::Create2 => Action::Create(CreateAction {
                from: self.trace.caller,
                value: self.trace.value,
                gas: self.trace.gas_limit,
                init: self.trace.data.clone(),
                creation_method: self.kind().into(),
            }),
        }
    }

    /// Converts this call trace into an _empty_ geth [CallFrame]
    pub fn geth_empty_call_frame(&self, include_logs: bool) -> CallFrame {
        let mut call_frame = CallFrame {
            typ: self.trace.kind.to_string(),
            from: self.trace.caller,
            to: Some(self.trace.address),
            value: Some(self.trace.value),
            gas: U256::from(self.trace.gas_limit),
            gas_used: U256::from(self.trace.gas_used),
            input: self.trace.data.clone(),
            output: (!self.trace.output.is_empty()).then(|| self.trace.output.clone()),
            error: None,
            revert_reason: None,
            calls: Default::default(),
            logs: Default::default(),
            ..Default::default()
        };

        if self.trace.kind.is_static_call() {
            // STATICCALL frames don't have a value
            call_frame.value = None;
        }
        if self.trace.frame_index.is_some() {
            call_frame.execution_gas_used = Some(U256::from(self.trace.gas_used));
            call_frame.state_gas_used = Some(U256::from(self.trace.state_gas_used));
        }

        // we need to populate error and revert reason
        if !self.trace.success {
            if self.kind().is_any_create() {
                call_frame.to = None;
            }

            if self.trace.frame_index.is_none()
                && !self.status().is_some_and(InstrStop::is_frame_revert)
            {
                call_frame.gas_used = U256::from(self.trace.gas_limit);
                call_frame.output = None;
            }

            call_frame.revert_reason = utils::maybe_revert_reason(self.trace.output.as_ref());

            // Note: regular calltracer uses geth errors, only flatCallTracer uses parity errors: <https://github.com/ethereum/go-ethereum/blob/a9523b6428238a762e1a1e55e46ead47630c3a23/eth/tracers/native/call_flat.go#L226>
            call_frame.error = self.trace.as_error_msg(TraceStyle::Geth);
        }

        if include_logs && !self.logs.is_empty() {
            call_frame.logs = self
                .logs
                .iter()
                .map(|log| CallLogFrame {
                    address: Some(log.address),
                    topics: Some(log.raw_log.topics().to_vec()),
                    data: Some(log.raw_log.data.clone()),
                    position: Some(log.position),
                    index: Some(log.index),
                })
                .collect();
        }

        call_frame
    }
}

/// A unified representation of a call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "UPPERCASE"))]
pub enum CallKind {
    /// Represents a regular call.
    #[default]
    Call,
    /// Represents a static call.
    StaticCall,
    /// Represents a call code operation.
    CallCode,
    /// Represents a delegate call.
    DelegateCall,
    /// Represents an authorized call.
    AuthCall,
    /// Represents a contract creation operation.
    Create,
    /// Represents a contract creation operation using the CREATE2 opcode.
    Create2,
}

impl CallKind {
    /// Returns the string representation of the call kind.
    pub const fn to_str(self) -> &'static str {
        match self {
            Self::Call => "CALL",
            Self::StaticCall => "STATICCALL",
            Self::CallCode => "CALLCODE",
            Self::DelegateCall => "DELEGATECALL",
            Self::AuthCall => "AUTHCALL",
            Self::Create => "CREATE",
            Self::Create2 => "CREATE2",
        }
    }

    /// Returns true if the call is a create
    #[inline]
    pub const fn is_any_create(&self) -> bool {
        matches!(self, Self::Create | Self::Create2)
    }

    /// Returns true if the call is a delegate of some sorts
    #[inline]
    pub const fn is_delegate(&self) -> bool {
        matches!(self, Self::DelegateCall | Self::CallCode)
    }

    /// Returns true if the call is [CallKind::StaticCall].
    #[inline]
    pub const fn is_static_call(&self) -> bool {
        matches!(self, Self::StaticCall)
    }

    /// Returns true if the call is [CallKind::AuthCall].
    #[inline]
    pub const fn is_auth_call(&self) -> bool {
        matches!(self, Self::AuthCall)
    }
}

impl From<CallKind> for CreationMethod {
    fn from(kind: CallKind) -> Self {
        match kind {
            CallKind::Create => Self::Create,
            CallKind::Create2 => Self::Create2,
            _ => Self::None,
        }
    }
}

impl core::fmt::Display for CallKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.to_str())
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

impl From<CallKind> for ActionType {
    fn from(kind: CallKind) -> Self {
        match kind {
            CallKind::Call
            | CallKind::StaticCall
            | CallKind::DelegateCall
            | CallKind::CallCode
            | CallKind::AuthCall => Self::Call,
            CallKind::Create | CallKind::Create2 => Self::Create,
        }
    }
}

impl From<CallKind> for CallType {
    fn from(ty: CallKind) -> Self {
        match ty {
            CallKind::Call => Self::Call,
            CallKind::StaticCall => Self::StaticCall,
            CallKind::CallCode => Self::CallCode,
            CallKind::DelegateCall => Self::DelegateCall,
            CallKind::Create | CallKind::Create2 => Self::None,
            CallKind::AuthCall => Self::AuthCall,
        }
    }
}

pub(crate) struct CallTraceStepStackItem<'a> {
    /// The trace node that contains this step
    pub(crate) trace_node: &'a CallTraceNode,
    /// The step that this stack item represents
    pub(crate) step: &'a CallTraceStep,
    /// The index of the child call in the CallArena if this step's opcode is a call
    pub(crate) call_child_id: Option<usize>,
}

/// Ordering enum for calls, logs and steps
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TraceMemberOrder {
    /// Contains the index of the corresponding log
    Log(usize),
    /// Contains the index of the corresponding trace node
    Call(usize),
    /// Contains the index of the corresponding step, if those are being traced
    Step(usize),
}

/// Represents a decoded internal function call.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DecodedInternalCall {
    /// Name of the internal function.
    pub func_name: String,
    /// Input arguments of the internal function.
    pub args: Option<Vec<String>>,
    /// Optional decoded return data.
    pub return_data: Option<Vec<String>>,
}

/// Represents a decoded trace step. Currently two formats are supported.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DecodedTraceStep {
    /// Decoded internal function call. Displayed similarly to external calls.
    ///
    /// Keeps decoded internal call data and an index of the step where the internal call execution
    /// ends.
    InternalCall(DecodedInternalCall, usize),
    /// Arbitrary line representing the step. Might be used for displaying individual opcodes.
    Line(String),
}

/// Represents a tracked call step during execution
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize),
    serde(from = "step_serde::CallTraceStepDe")
)]
pub struct CallTraceStep {
    // Fields filled in `step`
    /// Program counter before step execution
    pub pc: u32,
    /// Opcode to be executed
    pub op: OpCode,
    /// Stack before step execution
    pub stack: Option<Box<[U256]>>,
    /// The new stack items placed by this step if any
    pub push_stack: Option<Box<[U256]>>,
    /// Memory before step execution.
    ///
    /// This will be `None` only if memory capture is disabled.
    pub memory: Option<RecordedMemory>,
    /// Remaining gas before step execution
    pub gas_remaining: u64,
    /// Transaction-wide refund counter before step execution.
    ///
    /// Geth reads it after the gas function of the opcode, so its `SSTORE` and `SELFDESTRUCT`
    /// steps already include their own refund.
    pub gas_refund_counter: u64,
    /// The storage access of an SLOAD/SSTORE. Only set if state diffs are recorded.
    pub storage_change: Option<Box<StorageChange>>,
    // Fields filled in `step_end`
    /// Gas cost of step execution
    pub gas_cost: u64,
    /// State-gas reservoir before this step.
    pub state_gas_reservoir: Option<u64>,
    /// Final status of the step
    ///
    /// This is set after the step was executed.
    pub status: Option<InstrStop>,
    /// The data that most steps do not have, `None` if the step has none of it.
    pub extra: Option<Box<CallTraceStepExtra>>,
}

impl CallTraceStep {
    /// Converts this step into a geth [StructLog] with the given `stack`.
    ///
    /// This sets memory capture based on the `opts` parameter.
    pub(crate) fn convert_to_geth_struct_log(
        &self,
        opts: &GethDefaultTracingOptions,
        depth: u64,
        stack: Option<Vec<U256>>,
    ) -> StructLog {
        // A step that halts exceptionally costs 0, like geth and ethereum/execution-apis#986: the
        // attempted cost depends on where each client detects the failure.
        let halted = self.is_error();
        StructLog {
            depth,
            error: self.as_error(),
            gas: self.gas_remaining,
            gas_cost: if halted { 0 } else { self.gas_cost },
            state_gas_cost: self.state_gas_cost().filter(|_| !halted),
            // Like geth and ethereum/execution-apis#852, an empty reservoir is omitted.
            state_gas_reservoir: self.state_gas_reservoir.filter(|reservoir| *reservoir != 0),
            op: if self.op.is_valid() {
                self.op.as_str().into()
            } else {
                format!("opcode {:#x} not defined", self.op.get()).into()
            },
            pc: self.pc.into(),
            // geth omits a zero refund and empty memory and return data.
            refund_counter: (self.gas_refund_counter != 0).then_some(self.gas_refund_counter),
            stack,
            memory: if opts.is_memory_enabled() {
                self.memory
                    .as_ref()
                    .map(RecordedMemory::memory_chunks)
                    .filter(|memory| !memory.is_empty())
            } else {
                None
            },

            // Filled from external storage.
            storage: None,
            // Filled from `CallTraceNode`.
            return_data: None,

            // This is always `None` in the RPC response.
            memory_size: None,
        }
    }

    /// Returns true if the step is a call operation, any of
    /// CALL, CALLCODE, DELEGATECALL, STATICCALL, CREATE, CREATE2
    #[inline]
    pub(crate) const fn is_call_like_op(&self) -> bool {
        is_call_like(self.op)
    }

    // Reverts are control flow; only exceptional halts populate a step error.
    #[inline]
    pub(crate) const fn is_error(&self) -> bool {
        let Some(status) = self.status else {
            return false;
        };
        status.is_halt()
    }

    /// Returns the error message if it is an erroneous result.
    #[inline]
    pub(crate) fn as_error(&self) -> Option<String> {
        self.is_error().then(|| format!("{:?}", self.status))
    }

    /// Returns the returndata before step execution, empty if it was not recorded.
    #[inline]
    pub fn returndata(&self) -> &Bytes {
        static EMPTY: Bytes = Bytes::new();
        self.extra.as_deref().map_or(&EMPTY, |extra| &extra.returndata)
    }

    /// Returns the net state-gas change caused by this step.
    #[inline]
    pub fn state_gas_cost(&self) -> Option<i64> {
        self.extra.as_deref()?.state_gas_cost
    }

    /// Returns the immediate bytes of the step.
    #[inline]
    pub fn immediate_bytes(&self) -> Option<&Bytes> {
        self.extra.as_deref()?.immediate_bytes.as_ref()
    }

    /// Returns the complementary decoded step data.
    #[inline]
    pub fn decoded(&self) -> Option<&DecodedTraceStep> {
        self.extra.as_deref()?.decoded.as_deref()
    }

    /// Returns `DecodedTraceStep` from `CallTraceStep`.
    pub fn decoded_mut(&mut self) -> &mut DecodedTraceStep {
        self.extra
            .get_or_insert_default()
            .decoded
            .get_or_insert_with(|| Box::new(DecodedTraceStep::Line(String::new())))
    }
}

// A trace holds one step per executed opcode and one call trace per call.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<CallTraceStep>() == 128);
    assert!(size_of::<CallTrace>() == 296);
};

/// The data of a [`CallTraceStep`] that most steps do not have.
///
/// Boxed in the step, so that a step without it does not pay for its size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallTraceStepExtra {
    /// Returndata before step execution
    pub returndata: Bytes,
    /// Net state-gas change caused by this step.
    pub state_gas_cost: Option<i64>,
    /// Immediate bytes of the step
    pub immediate_bytes: Option<Bytes>,
    /// Optional complementary decoded step data.
    pub decoded: Option<Box<DecodedTraceStep>>,
}

impl CallTraceStepExtra {
    /// Boxes the data for [`CallTraceStep::extra`], `None` if all of it is empty.
    #[inline]
    pub fn boxed(self) -> Option<Box<Self>> {
        let empty = self.returndata.is_empty()
            && self.state_gas_cost.is_none()
            && self.immediate_bytes.is_none()
            && self.decoded.is_none();
        (!empty).then(|| Box::new(self))
    }
}

/// Whether the geth struct logger follows a child message started by this opcode.
pub(super) const fn is_call_like(op: OpCode) -> bool {
    matches!(
        op.get(),
        op::CALL | op::DELEGATECALL | op::STATICCALL | op::CREATE | op::CALLCODE | op::CREATE2
    )
}

/// The deltas a [`CallTraceStep`] produced, as reported by parity's `vmTrace`.
///
/// Recorded in [`CallTrace::step_deltas`] for the steps that report memory or storage, make a
/// call or gain gas. Memory and storage require [`record_step_deltas`]; gas balances are recorded
/// independently.
///
/// [`record_step_deltas`]: crate::tracing::TracingInspectorConfig::record_step_deltas
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StepDelta {
    /// The index of the step in [`CallTrace::steps`].
    pub step: usize,
    /// The memory the step wrote, or the word an `MLOAD` read, as it is after the step.
    pub memory: Option<MemoryDelta>,
    /// The storage written by an `SSTORE`, taken from its operands, so it is also set when the
    /// value does not change.
    pub store: Option<StorageDelta>,
    /// The remaining gas after a call-like step or a step whose remaining gas after execution is
    /// not `gas_remaining - gas_cost`, such as one that gained gas or drew state gas from it.
    ///
    /// For all other steps the remaining gas after execution is `gas_remaining - gas_cost`.
    pub gas_remaining_after: Option<u64>,
    /// The memory range the step reports, captured before execution and consumed once its
    /// contents are recorded.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) write_range: Option<Range<usize>>,
}

impl StepDelta {
    /// Records the contents of the captured memory range after the step, if one was captured.
    pub(crate) fn record_memory_write(
        &mut self,
        memory: &[u8],
        budget: &mut super::limits::TraceBudget,
    ) {
        if let Some(range) = self.write_range.take().filter(|range| !range.is_empty()) {
            self.memory = memory
                .get(range.clone())
                .filter(|data| budget.reserve(data.len()))
                .map(|data| MemoryDelta { off: range.start, data: Bytes::copy_from_slice(data) });
        }
    }
}

/// Represents the source of a storage change - e.g., whether it came
/// from an SSTORE or SLOAD instruction.
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StorageChangeReason {
    /// SLOAD opcode
    SLOAD,
    /// SSTORE opcode
    SSTORE,
}

/// Represents the storage access of an SLOAD or SSTORE step, read before the opcode runs.
///
/// For an SLOAD, `value` is the value the slot holds and `had_value` is `None`. For an SSTORE,
/// `value` is the value written and `had_value` is the value the slot held before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StorageChange {
    /// key of the storage slot
    pub key: U256,
    /// The value read by an SLOAD, or written by an SSTORE
    pub value: U256,
    /// The value an SSTORE overwrites, `None` for an SLOAD
    pub had_value: Option<U256>,
    /// How this storage was accessed
    pub reason: StorageChangeReason,
}

/// Represents the memory captured during execution
///
/// This is a wrapper around the [Memory](evm2::interpreter::Memory) context memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecordedMemory(pub(crate) Bytes);

impl RecordedMemory {
    #[inline]
    pub(crate) fn new(mem: &[u8]) -> Self {
        Self(Bytes::copy_from_slice(mem))
    }

    /// Returns the memory as a byte slice
    #[inline]
    pub const fn as_bytes(&self) -> &Bytes {
        &self.0
    }

    /// Returns the memory as a byte vector
    #[inline]
    pub fn into_bytes(self) -> Bytes {
        self.0
    }

    /// Returns the size of the memory.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the memory is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Formats memory data into a list of 32-byte hex-encoded chunks.
    ///
    /// See: <https://github.com/ethereum/go-ethereum/blob/366d2169fbc0e0f803b68c042b77b6b480836dbc/eth/tracers/logger/logger.go#L450-L452>
    #[inline]
    pub fn memory_chunks(&self) -> Vec<String> {
        convert_memory(self.as_bytes())
    }
}

impl AsRef<[u8]> for RecordedMemory {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Serializes a [`CallTrace`] as one flat struct, independently of its in-memory layout.
#[cfg(feature = "serde")]
mod trace_serde {
    use super::*;
    use serde::{Deserialize, Serialize, Serializer};

    #[derive(Serialize)]
    #[serde(rename = "CallTrace")]
    struct CallTraceSer<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        frame_transaction_root: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entered_evm: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        frame_index: Option<Option<usize>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        frame_mode: Option<Option<u8>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        state_gas_used: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rolled_back: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped: Option<bool>,
        depth: usize,
        success: bool,
        caller: &'a Address,
        address: &'a Address,
        maybe_precompile: Option<bool>,
        selfdestruct_address: Option<Address>,
        selfdestruct_refund_target: Option<Address>,
        selfdestruct_transferred_value: Option<U256>,
        kind: CallKind,
        value: &'a U256,
        data: &'a Bytes,
        output: &'a Bytes,
        bytecode: &'a Option<Bytes>,
        gas_used: u64,
        gas_limit: u64,
        gas_refund_counter: u64,
        status: Option<InstrStop>,
        steps: &'a [CallTraceStep],
        step_deltas: &'a [StepDelta],
        decoded: &'a Option<Box<DecodedCallTrace>>,
    }

    #[derive(Deserialize)]
    #[serde(rename = "CallTrace")]
    pub(super) struct CallTraceDe {
        #[serde(default)]
        frame_transaction_root: bool,
        #[serde(default)]
        entered_evm: bool,
        #[serde(default)]
        frame_index: Option<usize>,
        #[serde(default)]
        frame_mode: Option<u8>,
        #[serde(default)]
        state_gas_used: u64,
        #[serde(default)]
        rolled_back: bool,
        #[serde(default)]
        skipped: bool,
        depth: usize,
        success: bool,
        caller: Address,
        address: Address,
        maybe_precompile: Option<bool>,
        selfdestruct_address: Option<Address>,
        selfdestruct_refund_target: Option<Address>,
        selfdestruct_transferred_value: Option<U256>,
        kind: CallKind,
        value: U256,
        data: Bytes,
        output: Bytes,
        #[serde(default)]
        bytecode: Option<Bytes>,
        gas_used: u64,
        gas_limit: u64,
        gas_refund_counter: u64,
        status: Option<InstrStop>,
        steps: Vec<CallTraceStep>,
        #[serde(default)]
        step_deltas: Vec<StepDelta>,
        decoded: Option<Box<DecodedCallTrace>>,
    }

    impl Serialize for CallTrace {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let binary = !serializer.is_human_readable();
            CallTraceSer {
                frame_transaction_root: (binary || self.frame_transaction_root)
                    .then_some(self.frame_transaction_root),
                entered_evm: (binary || self.entered_evm).then_some(self.entered_evm),
                frame_index: (binary || self.frame_index.is_some()).then_some(self.frame_index),
                frame_mode: (binary || self.frame_mode.is_some()).then_some(self.frame_mode),
                state_gas_used: (binary || self.state_gas_used != 0).then_some(self.state_gas_used),
                rolled_back: (binary || self.rolled_back).then_some(self.rolled_back),
                skipped: (binary || self.skipped).then_some(self.skipped),
                depth: self.depth.into(),
                success: self.success,
                caller: &self.caller,
                address: &self.address,
                maybe_precompile: self.maybe_precompile,
                selfdestruct_address: self.selfdestruct_address(),
                selfdestruct_refund_target: self.selfdestruct_refund_target(),
                selfdestruct_transferred_value: self.selfdestruct_transferred_value(),
                kind: self.kind,
                value: &self.value,
                data: &self.data,
                output: &self.output,
                bytecode: &self.bytecode,
                gas_used: self.gas_used,
                gas_limit: self.gas_limit,
                gas_refund_counter: self.gas_refund_counter,
                status: self.status,
                steps: &self.steps,
                step_deltas: &self.step_deltas,
                decoded: &self.decoded,
            }
            .serialize(serializer)
        }
    }

    impl From<CallTraceDe> for CallTrace {
        fn from(trace: CallTraceDe) -> Self {
            let selfdestruct = SelfdestructTrace {
                address: trace.selfdestruct_address,
                refund_target: trace.selfdestruct_refund_target,
                transferred_value: trace.selfdestruct_transferred_value,
            };
            Self {
                frame_transaction_root: trace.frame_transaction_root,
                entered_evm: trace.entered_evm,
                frame_index: trace.frame_index,
                frame_mode: trace.frame_mode,
                state_gas_used: trace.state_gas_used,
                rolled_back: trace.rolled_back,
                skipped: trace.skipped,
                // A depth above the call depth limit is not a recorded trace.
                depth: trace.depth.try_into().unwrap_or(u16::MAX),
                success: trace.success,
                caller: trace.caller,
                address: trace.address,
                maybe_precompile: trace.maybe_precompile,
                selfdestruct: (selfdestruct != SelfdestructTrace::default())
                    .then(|| Box::new(selfdestruct)),
                kind: trace.kind,
                value: trace.value,
                data: trace.data,
                output: trace.output,
                bytecode: trace.bytecode,
                gas_used: trace.gas_used,
                gas_limit: trace.gas_limit,
                gas_refund_counter: trace.gas_refund_counter,
                status: trace.status,
                steps: trace.steps,
                step_deltas: trace.step_deltas,
                decoded: trace.decoded,
            }
        }
    }
}

/// Serializes a [`CallTraceStep`] as one flat struct, independently of its in-memory layout.
#[cfg(feature = "serde")]
mod step_serde {
    use super::*;
    use serde::{Deserialize, Serialize, Serializer};

    #[derive(Serialize)]
    #[serde(rename = "CallTraceStep")]
    struct CallTraceStepSer<'a> {
        pc: usize,
        #[serde(with = "opcode_serde")]
        op: OpCode,
        stack: &'a Option<Box<[U256]>>,
        push_stack: &'a Option<Box<[U256]>>,
        memory: &'a Option<RecordedMemory>,
        returndata: &'a Bytes,
        gas_remaining: u64,
        gas_refund_counter: u64,
        storage_change: &'a Option<Box<StorageChange>>,
        gas_cost: u64,
        state_gas_cost: Option<i64>,
        state_gas_reservoir: Option<u64>,
        status: Option<InstrStop>,
        immediate_bytes: Option<&'a Bytes>,
        decoded: Option<&'a DecodedTraceStep>,
    }

    #[derive(Deserialize)]
    #[serde(rename = "CallTraceStep")]
    pub(super) struct CallTraceStepDe {
        pc: usize,
        #[serde(with = "opcode_serde")]
        op: OpCode,
        stack: Option<Box<[U256]>>,
        push_stack: Option<Box<[U256]>>,
        memory: Option<RecordedMemory>,
        returndata: Bytes,
        gas_remaining: u64,
        gas_refund_counter: u64,
        storage_change: Option<Box<StorageChange>>,
        gas_cost: u64,
        state_gas_cost: Option<i64>,
        state_gas_reservoir: Option<u64>,
        status: Option<InstrStop>,
        immediate_bytes: Option<Bytes>,
        decoded: Option<Box<DecodedTraceStep>>,
    }

    impl Serialize for CallTraceStep {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            CallTraceStepSer {
                pc: self.pc as usize,
                op: self.op,
                stack: &self.stack,
                push_stack: &self.push_stack,
                memory: &self.memory,
                returndata: self.returndata(),
                gas_remaining: self.gas_remaining,
                gas_refund_counter: self.gas_refund_counter,
                storage_change: &self.storage_change,
                gas_cost: self.gas_cost,
                state_gas_cost: self.state_gas_cost(),
                state_gas_reservoir: self.state_gas_reservoir,
                status: self.status,
                immediate_bytes: self.immediate_bytes(),
                decoded: self.decoded(),
            }
            .serialize(serializer)
        }
    }

    impl From<CallTraceStepDe> for CallTraceStep {
        fn from(step: CallTraceStepDe) -> Self {
            Self {
                // Code that large cannot be executed.
                pc: step.pc.try_into().unwrap_or(u32::MAX),
                op: step.op,
                stack: step.stack,
                push_stack: step.push_stack,
                memory: step.memory,
                gas_remaining: step.gas_remaining,
                gas_refund_counter: step.gas_refund_counter,
                storage_change: step.storage_change,
                gas_cost: step.gas_cost,
                state_gas_reservoir: step.state_gas_reservoir,
                status: step.status,
                extra: CallTraceStepExtra {
                    returndata: step.returndata,
                    state_gas_cost: step.state_gas_cost,
                    immediate_bytes: step.immediate_bytes,
                    decoded: step.decoded,
                }
                .boxed(),
            }
        }
    }
}

#[cfg(feature = "serde")]
mod opcode_serde {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(op: &OpCode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(op.get())
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<OpCode, D::Error>
    where
        D: Deserializer<'de>,
    {
        let op = u8::deserialize(deserializer)?;
        Ok(OpCode::new(op).unwrap_or_else(|| OpCode::new(op::INVALID).unwrap()))
    }
}

#[cfg(all(test, feature = "serde"))]
mod frame_serde_tests {
    use super::*;

    #[test]
    fn call_trace_roundtrip_preserves_frame_metadata() {
        let trace = CallTrace {
            frame_transaction_root: true,
            entered_evm: true,
            frame_index: Some(2),
            frame_mode: Some(3),
            state_gas_used: 42,
            rolled_back: true,
            skipped: true,
            ..Default::default()
        };
        for trace in [CallTrace::default(), trace] {
            let json = serde_json::to_vec(&trace).unwrap();
            assert_eq!(serde_json::from_slice::<CallTrace>(&json).unwrap(), trace);
            for bytes in
                [rmp_serde::to_vec(&trace).unwrap(), rmp_serde::to_vec_named(&trace).unwrap()]
            {
                assert_eq!(rmp_serde::from_slice::<CallTrace>(&bytes).unwrap(), trace);
            }
        }
    }
}
