//! Geth struct logs written as JSON while the transaction executes.

use crate::tracing::{
    TraceLimits,
    builder::geth::{struct_log_header_len, struct_log_stack_len},
    config::geth_step_limit,
    limits::TraceBudget,
    step_refund_counter, storage_change,
    types::is_call_like,
};
use alloc::{boxed::Box, collections::BTreeMap, format, string::String, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, U256, hex, map::AddressMap};
use alloy_rpc_types_trace::geth::GethDefaultTracingOptions;
use core::{mem, ops::Range};
use evm2::{
    EvmFeatures, EvmTypes, Inspector, TxResultExt,
    interpreter::{InstrStop, Interpreter, Message, MessageResult, opcode::OpCode},
};
use serde_json::value::RawValue;

#[cfg(feature = "serde")]
use alloy_primitives::U64;
#[cfg(feature = "serde")]
use serde::ser::{Serialize, SerializeStruct, Serializer};

/// Records the struct logs of geth's default tracer as JSON, without retaining the steps.
///
/// [`TracingInspector`](super::TracingInspector) records every step, its builder converts the
/// steps to [`StructLog`](alloy_rpc_types_trace::geth::StructLog)s and the caller serializes
/// those. This inspector writes the JSON of each struct log from the step hooks instead, which is
/// several times faster and retains nothing but the JSON. [`Self::take_frame`] returns a
/// [`RawDefaultFrame`], which serializes to the same JSON as the
/// [`DefaultFrame`](alloy_rpc_types_trace::geth::DefaultFrame) of the typed path.
///
/// A struct log is written once its gas cost is known: when the step ends, or when the message
/// it started begins, so that the struct logs of that message follow it. The state gas cost of
/// such a step, which a halt at its end drops, and its error are inserted when the frame is taken.
/// The final byte cutoff uses the completed sizes in execution order.
///
/// The inspector can be reused for multiple transactions by calling [`Self::fuse`] after each.
#[derive(Clone, Debug)]
pub struct RawStructLogInspector {
    /// The options of the struct logger, with a zero `limit` normalized to `None`.
    opts: GethDefaultTracingOptions,
    /// Tracks the bytes of JSON written.
    budget: TraceBudget,
    /// Cumulative recording bytes charged before the current depth-zero message.
    root_start_bytes: usize,
    /// The JSON array of the struct logs written so far, without its closing bracket.
    logs: Vec<u8>,
    /// The capacity to reserve for the struct logs of a transaction, from the size of those of
    /// the previous transactions. Each frame takes its struct logs along, so their buffer would
    /// otherwise grow from nothing for every transaction.
    capacity_hint: usize,
    /// The bytes of JSON of the struct logs written so far, without the array syntax, which geth
    /// counts against the `limit` option.
    json_len: u64,
    /// Remaining step hooks under the geth capture limit, including hidden frames.
    steps_left: Option<u64>,
    /// Message depths observed on entry. A hidden frame and its descendants carry `None`.
    frames: Vec<Option<u16>>,
    /// The running step, unless a message it started is running.
    step: StepState,
    /// The storage slot the running step accesses.
    storage_access: Option<StorageAccess>,
    /// The fields of the running step that follow its error: the stack, the return data and the
    /// memory.
    tail: Tail,
    /// Visible call steps waiting for their step_end hook, innermost last.
    open_steps: Vec<OpenStep>,
    /// The fields of written struct logs that were only known later.
    patches: Patches,
    /// The storage slots each contract has loaded and stored so far.
    storage: AddressMap<BTreeMap<B256, B256>>,
    /// Whether the top-level message failed.
    failed: bool,
    /// The active EVM features.
    ///
    /// This is filled during execution.
    features: EvmFeatures,
}

impl RawStructLogInspector {
    /// Returns a new instance for the given options.
    pub fn new(opts: GethDefaultTracingOptions) -> Self {
        Self {
            opts: GethDefaultTracingOptions {
                limit: opts.limit.filter(|limit| *limit != 0),
                ..opts
            },
            budget: TraceBudget::default(),
            root_start_bytes: 0,
            logs: Vec::new(),
            capacity_hint: 0,
            json_len: 0,
            frames: Vec::new(),
            steps_left: geth_step_limit(&opts).map(|n| n.get()),
            step: StepState::None,
            storage_access: None,
            tail: Tail::default(),
            open_steps: Vec::new(),
            patches: Patches::default(),
            storage: AddressMap::default(),
            failed: true,
            features: EvmFeatures::empty(),
        }
    }

    /// Sets the byte budget for the struct logs, and keeps it across [`Self::fuse`].
    ///
    /// The budget counts the JSON bytes of each complete struct log. The first log that would
    /// exceed it and every following log are omitted. If a pending call can move the geth
    /// cutoff ahead of that log, recording stops immediately and a requested halt waits until
    /// the cutoff is known.
    pub const fn set_limits(&mut self, limits: TraceLimits) {
        self.budget.limits = limits;
    }

    /// Creates an empty recorder with the same options and limits.
    pub(super) fn fresh(&self) -> Self {
        let mut fresh = Self::new(self.opts);
        fresh.set_limits(self.budget.limits);
        fresh
    }

    /// Returns whether struct logs were left out after reaching the byte budget.
    pub const fn limit_exceeded(&self) -> bool {
        self.budget.exceeded()
    }

    /// Returns the JSON bytes of the struct logs counted against the budget since the last
    /// [`Self::fuse`].
    pub const fn recorded_bytes(&self) -> usize {
        self.budget.recorded
    }

    /// Resets the inspector to its initial state of [`Self::new`], keeping its limits.
    ///
    /// Note that this method has no effect on the allocated capacity of its buffers.
    pub fn fuse(&mut self) {
        let Self {
            logs,
            root_start_bytes,
            json_len,
            frames,
            steps_left,
            step,
            storage_access,
            tail,
            open_steps,
            patches,
            storage,
            failed,
            features,
            // kept
            opts,
            capacity_hint: _,
            budget,
        } = self;
        logs.clear();
        *root_start_bytes = 0;
        *json_len = 0;
        frames.clear();
        *steps_left = geth_step_limit(opts).map(|n| n.get());
        *step = StepState::None;
        *storage_access = None;
        tail.clear();
        tail.buf.clear();
        open_steps.clear();
        patches.clear();
        storage.clear();
        *failed = true;
        *features = EvmFeatures::empty();
        budget.recorded = 0;
        budget.exceeded = false;
    }

    /// Takes the struct logs written for the executed transaction and returns its frame.
    ///
    /// Calling this again before the next transaction returns a frame without struct logs.
    pub fn take_frame<E>(&mut self, result: &TxResultExt<E>) -> RawDefaultFrame {
        if !self.patches.entries.is_empty() {
            self.finalize_logs();
        }
        let mut logs = mem::take(&mut self.logs);
        self.open_steps.clear();
        self.json_len = 0;
        if logs.is_empty() {
            logs.push(b'[');
        }
        logs.push(b']');
        // The hint decays, so that one large transaction does not make every later one reserve
        // as much.
        self.capacity_hint = logs.len().max(self.capacity_hint / 2);
        // SAFETY: Only whole strings are appended to the struct logs: literals, digits, opcode
        // names and strings formatted by `alloc` and `serde_json`.
        let logs = unsafe { String::from_utf8_unchecked(logs) };
        // SAFETY: The struct logs are a JSON array of objects without surrounding whitespace. The
        // keys are literals, the values are numbers, hex strings, opcode names, which need no
        // escaping, and strings serialized by `serde_json`.
        let struct_logs = unsafe { RawValue::from_string_unchecked(logs) };

        let eip8037 = self.features.contains(EvmFeatures::EIP8037);
        RawDefaultFrame {
            failed: self.failed,
            gas: result.tx_gas_used(),
            execution_gas_used: eip8037.then(|| result.execution_gas_spent()),
            state_gas_used: eip8037.then(|| result.state_gas_spent()),
            gas_refund: eip8037.then(|| result.final_refunded()),
            return_value: result.output.clone(),
            struct_logs,
        }
    }

    /// Applies deferred fields and enforces both byte cutoffs on the completed prefix.
    fn finalize_logs(&mut self) {
        self.patches.apply(&mut self.logs);
        let budget = self.root_byte_budget();
        while !self.logs.is_empty() {
            let over_budget = budget.is_some_and(|max| self.json_len > max as u64);
            if !over_budget && self.opts.limit.is_none_or(|limit| self.json_len <= limit) {
                break;
            }
            // Every log begins with this literal. Only a log separator can match it.
            const NEXT_LOG: &[u8] = b",{\"pc\":";
            let start = self.logs.windows(NEXT_LOG.len()).rposition(|w| w == NEXT_LOG).unwrap_or(0);
            let before = self.json_len - (self.logs.len() - start - 1) as u64;
            // The geth limit includes the first log that crosses it. The recording budget
            // excludes it.
            if !over_budget && self.opts.limit.is_none_or(|limit| before <= limit) {
                break;
            }
            self.logs.truncate(start);
            self.json_len = before;
        }
    }

    fn root_byte_budget(&self) -> Option<usize> {
        self.budget.limits.max_recorded_bytes.map(|max| max.saturating_sub(self.root_start_bytes))
    }

    /// A pending parent can put the first omitted child beyond the geth cutoff. Stop copying
    /// as soon as the budget is full, then decide whether to halt once that parent is final.
    #[inline]
    fn check_budget<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>, in_step: bool) {
        if !self.budget.exceeded() {
            return;
        }
        if let Some(limit) = self.opts.limit
            && self.root_byte_budget().is_some_and(|max| limit < max as u64)
        {
            if !self.open_steps.is_empty() {
                let growth = (self.open_steps.len() as u64).saturating_mul(MAX_PATCH_GROWTH);
                if self.json_len.saturating_add(growth) > limit {
                    return;
                }
            } else if self.json_len > limit {
                self.finalize_logs();
                if self.json_len > limit {
                    // The complete geth prefix fits within the recording budget.
                    self.budget.recorded = self.root_start_bytes + self.json_len as usize;
                    self.budget.exceeded = false;
                    return;
                }
            }
        }
        self.budget.halt(interp, in_step);
    }

    /// Starts recording a step.
    ///
    /// Invoked on [`Inspector::step`].
    fn start_step<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>, depth: u16) {
        self.storage_access = None;
        // geth stops logging once the JSON written so far is over the limit.
        if self.budget.exceeded() || self.opts.limit.is_some_and(|limit| self.json_len > limit) {
            self.step = StepState::Skipped;
            return;
        }

        let op = OpCode::new_or_unknown(interp.opcode());
        let refund = step_refund_counter(interp);
        let gas = interp.gas();
        let mut step = RecordingStep {
            pc: interp.pc() as u64,
            op,
            gas: gas.remaining(),
            gas_before: gas.remaining_excluding_state_spill(),
            message_charge: None,
            depth: u64::from(depth) + 1,
            refund,
            state_gas_spent: gas.state_gas_spent(),
            state_gas_reservoir: interp
                .version()
                .feature(EvmFeatures::EIP8037)
                .then_some(gas.reservoir()),
            reserved: 0,
        };
        let stack = interp.stack();
        let stack = self.opts.is_stack_enabled().then(|| stack.as_slice());
        let return_data = interp.return_data().as_ref();
        let return_data =
            (self.opts.is_return_data_enabled() && !return_data.is_empty()).then_some(return_data);
        let memory = interp.memory();
        let memory = (self.opts.is_memory_enabled() && !memory.is_empty())
            .then(|| memory.slice(0, memory.len()));
        // Charge fields known at entry before reserving scratch space or executing the opcode.
        // Even a zero gas cost takes one digit. Outcome-dependent fields can only add bytes.
        if self.budget.limits.max_recorded_bytes.is_some() {
            let op_len = if op.is_valid() {
                op.as_str().len()
            } else {
                format!("opcode {:#x} not defined", op.get()).len()
            };
            let mut len = struct_log_header_len(
                step.pc,
                op_len,
                step.gas,
                0,
                step.depth,
                step.reported_reservoir(),
                step.refund,
            );
            len += stack.map_or(0, struct_log_stack_len);
            len += return_data.map_or(0, Tail::return_data_len);
            len += memory.map_or(0, Tail::memory_len);
            if !self.budget.reserve(len) {
                self.step = StepState::Skipped;
                return;
            }
            step.reserved = len;
        }
        self.tail.clear();
        if let Some(stack) = stack {
            self.tail.push_stack(stack);
        }
        if let Some(data) = return_data {
            self.tail.push_return_data(data);
        }
        if let Some(memory) = memory {
            self.tail.push_memory(memory);
        }
        if self.opts.is_storage_enabled() {
            self.storage_access = storage_change(interp, op.get()).map(|change| StorageAccess {
                address: interp.message().destination,
                key: change.key,
                value: change.value,
            });
        }

        self.step = StepState::Recording(step);
    }

    /// Writes the struct log of the running step, unless a message it started has written it.
    ///
    /// Invoked on [`Inspector::step_end`].
    fn end_step<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let result = interp.result();
        match mem::take(&mut self.step) {
            StepState::Recording(step) => {
                // A step that halts exceptionally costs 0, like a typed struct log.
                let (cost, state_cost) = if result.is_err_and(InstrStop::is_halt) {
                    (0, None)
                } else if let Some(charge) = step.message_charge {
                    (charge.cost, charge.state_cost)
                } else {
                    let gas = interp.gas();
                    let charged =
                        step.gas_before.saturating_sub(gas.remaining_excluding_state_spill());
                    (charged, step.state_gas_cost(gas.state_gas_spent()))
                };
                self.write_log(&step, cost, state_cost, result.err());
            }
            StepState::Skipped => {}
            StepState::None => {
                let Some(open) = self.open_steps.pop() else { return };
                let halted = result.is_err_and(InstrStop::is_halt);
                let state_cost = open.charge.state_cost.filter(|_| !halted);
                let error = result.err().filter(|status| status.is_halt()).and_then(error_json);
                if state_cost.is_none() && error.is_none() && !halted {
                    return;
                }
                // A step that halts exceptionally costs 0, like a typed struct log. This includes
                // the reentrancy sentry, which rejects the instruction before charging it.
                let cost = if halted { 0 } else { open.charge.cost };
                let added = self.patches.replace(open.fields, |patch| {
                    let mut fields = ArrayBuf::<HEAD_CAPACITY>::new();
                    open.step.write_cost_and_depth(&mut fields, cost, state_cost);
                    patch.extend_from_slice(fields.as_slice());
                    if let Some(error) = error {
                        patch.extend_from_slice(ERROR_KEY);
                        patch.extend_from_slice(error.as_bytes());
                    }
                });
                debug_assert!(added as u64 <= MAX_PATCH_GROWTH);
                self.json_len += added as u64;
                self.budget.reserve(added);
            }
        }
    }

    /// Writes the struct log of the running step, which has charged all it charges before the
    /// message it starts begins.
    ///
    /// Invoked on [`Inspector::call`] and [`Inspector::create`]. A call step costs the gas
    /// forwarded to the child too, a create step does not, so `forwarded` is the gas to give back.
    fn start_message<T: EvmTypes>(
        &mut self,
        interp: &Interpreter<'_, '_, T>,
        message: &Message<T>,
        forwarded: u64,
    ) {
        self.features = interp.version().features;
        if self.frames.last() == Some(&None) {
            self.frames.push(None);
            return;
        }
        if self.frames.is_empty() && message.depth == 0 {
            // The typed arena replaces its root. The capture limits span the whole transaction.
            self.logs.clear();
            self.patches.clear();
            self.storage.clear();
            self.json_len = 0;
            self.root_start_bytes = self.budget.recorded;
            self.failed = true;
            self.frames.push(Some(message.depth));
            return;
        }
        if let StepState::Recording(step) = &mut self.step {
            let gas = interp.gas();
            let after = gas.remaining_excluding_state_spill().saturating_add(forwarded);
            // The state gas of the message and the refill of the step's charge when the message
            // fails are not part of the step's state gas cost.
            let charge = MessageCharge {
                cost: step.gas_before.saturating_sub(after),
                state_cost: step.state_gas_cost(gas.state_gas_spent()),
            };
            if is_call_like(step.op) {
                let step = *step;
                if let Some(fields) = self.write_log(&step, charge.cost, None, None) {
                    self.open_steps.push(OpenStep { step, charge, fields });
                    self.step = StepState::None;
                    self.frames.push(Some(message.depth));
                    return;
                }
                self.step = StepState::Skipped;
            } else {
                step.message_charge.get_or_insert(charge);
            }
        }
        // A parentless message above depth zero, a native precompile subcall, or a child of
        // an ordinary opcode has no corresponding call step in the typed trace.
        self.frames.push(None);
    }

    /// Finalizes a frame, including a step that a composed inspector stopped before execution.
    fn end_message<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>, success: bool) {
        // A later inspector may edit the message, including its depth, before it returns.
        if !matches!(self.frames.pop(), Some(Some(_))) {
            return;
        }
        if let StepState::Recording(step) = mem::take(&mut self.step) {
            self.write_log(&step, 0, None, None);
            self.check_budget(interp, false);
        }
        if self.frames.is_empty() {
            self.failed = !success;
            // Only the completed geth prefix counts against the budget of subsequent roots.
            if !self.budget.exceeded()
                && self.opts.limit.is_some_and(|limit| self.json_len > limit)
                && !self.patches.entries.is_empty()
            {
                self.finalize_logs();
                self.budget.recorded = self.root_start_bytes + self.json_len as usize;
            }
        }
    }

    /// Appends the struct log of `step` to the struct logs, unless it exceeds the byte budget.
    ///
    /// Returns the range containing the fields that may change after a child returns.
    fn write_log(
        &mut self,
        step: &RecordingStep,
        cost: u64,
        state_gas_cost: Option<i64>,
        status: Option<InstrStop>,
    ) -> Option<Range<usize>> {
        // The separator from the previous struct log is not part of this one.
        let mut head = ArrayBuf::<HEAD_CAPACITY>::new();
        head.bytes(if self.logs.is_empty() { b"[" } else { b"," });
        head.bytes(b"{\"pc\":");
        head.u64(step.pc);
        head.bytes(b",\"op\":\"");
        if step.op.is_valid() {
            let (name, len) = &OPCODE_NAMES[usize::from(step.op.get())];
            head.padded(name, usize::from(*len));
        } else {
            head.bytes(format!("opcode {:#x} not defined", step.op.get()).as_bytes());
        }
        head.bytes(b"\",\"gas\":");
        head.u64(step.gas);
        head.bytes(b",\"gasCost\":");
        let cost_at = head.len;
        step.write_cost_and_depth(&mut head, cost, state_gas_cost);
        let error_at = head.len;

        // Reverts are control flow, only exceptional halts are errors.
        let error = status.filter(|status| status.is_halt()).and_then(error_json);
        // geth snapshots the storage of the contract, with the slots of all its earlier SLOADs
        // and SSTOREs, unless the opcode fails before it runs.
        let storage = if error.is_none()
            && let Some(StorageAccess { address, key, value }) = self.storage_access.take()
        {
            let storage = self.storage.entry(address).or_default();
            storage.insert(key.into(), value.into());
            Some(&*storage)
        } else {
            None
        };
        let mut end = ArrayBuf::<32>::new();
        if step.refund != 0 {
            end.bytes(b",\"refund\":");
            end.u64(step.refund);
        }
        end.bytes(b"}");

        let len = head.len - 1
            + error.as_ref().map_or(0, |error| ERROR_KEY.len() + error.len())
            + self.tail.len
            + storage.map_or(0, |storage| {
                STORAGE_KEY.len() + storage.len() * (STORAGE_ENTRY_LEN + 1) + 1
            })
            + end.len;
        if !self.budget.reserve(len - step.reserved) {
            return None;
        }

        if self.logs.is_empty() {
            self.logs.reserve(self.capacity_hint);
        }
        let start = self.logs.len();
        if error.is_none() && self.tail.len == 0 && storage.is_none() {
            // Most struct logs of a logger without stacks are appended at once.
            head.bytes(end.as_slice());
            head.append_to(&mut self.logs);
        } else {
            head.append_to(&mut self.logs);
            if let Some(error) = &error {
                self.logs.extend_from_slice(ERROR_KEY);
                self.logs.extend_from_slice(error.as_bytes());
            }
            self.logs.extend_from_slice(self.tail.as_slice());
            if let Some(storage) = storage {
                self.logs.extend_from_slice(STORAGE_KEY);
                for (index, (key, value)) in storage.iter().enumerate() {
                    let mut entry = [b'"'; STORAGE_ENTRY_LEN + 1];
                    entry[0] = if index == 0 { b'{' } else { b',' };
                    entry[2..68]
                        .copy_from_slice(hex::Buffer::<32, true>::new().format(&key.0).as_bytes());
                    entry[69] = b':';
                    entry[71..137].copy_from_slice(
                        hex::Buffer::<32, true>::new().format(&value.0).as_bytes(),
                    );
                    self.logs.extend_from_slice(&entry);
                }
                self.logs.push(b'}');
            }
            self.logs.extend_from_slice(end.as_slice());
        }
        debug_assert_eq!(self.logs.len() - start - 1, len);

        self.json_len += len as u64;
        Some(start + cost_at..start + error_at)
    }
}

impl<T: EvmTypes> Inspector<T> for RawStructLogInspector {
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.features = interp.version().features;
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if let Some(left) = &mut self.steps_left {
            if *left == 0 {
                if matches!(self.frames.last(), Some(Some(_))) {
                    self.step = StepState::Skipped;
                }
                return;
            }
            *left -= 1;
        }
        let Some(&Some(depth)) = self.frames.last() else { return };
        self.start_step(interp, depth);
        self.check_budget(interp, true);
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if !matches!(self.frames.last(), Some(Some(_))) {
            return;
        }
        self.end_step(interp);
        self.check_budget(interp, true);
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.start_message(interp, message, 0);
        self.check_budget(interp, false);
        None
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.end_message(interp, result.is_success());
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.start_message(interp, message, message.gas_limit);
        self.check_budget(interp, false);
        None
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.end_message(interp, result.is_success());
    }
}

/// The frame of geth's default struct logger with its struct logs as JSON.
///
/// Returned by [`RawStructLogInspector::take_frame`]. It serializes to the same JSON as a
/// [`DefaultFrame`](alloy_rpc_types_trace::geth::DefaultFrame).
#[derive(Clone, Debug)]
pub struct RawDefaultFrame {
    /// Whether the transaction failed.
    pub failed: bool,
    /// The gas used by the transaction, as its receipt reports it.
    pub gas: u64,
    /// The execution gas used by the transaction, reported once EIP-8037 is active.
    pub execution_gas_used: Option<u64>,
    /// The state gas used by the transaction, reported once EIP-8037 is active.
    pub state_gas_used: Option<u64>,
    /// The gas refunded to the transaction, reported once EIP-8037 is active.
    pub gas_refund: Option<u64>,
    /// The output of the transaction.
    pub return_value: Bytes,
    /// The JSON array of the struct logs.
    pub struct_logs: Box<RawValue>,
}

#[cfg(feature = "serde")]
impl Serialize for RawDefaultFrame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let gas_fields = [
            ("executionGasUsed", self.execution_gas_used),
            ("stateGasUsed", self.state_gas_used),
            ("gasRefund", self.gas_refund),
        ];
        let count = 4 + gas_fields.iter().filter(|(_, gas)| gas.is_some()).count();
        let mut frame = serializer.serialize_struct("RawDefaultFrame", count)?;
        frame.serialize_field("failed", &self.failed)?;
        frame.serialize_field("gas", &self.gas)?;
        for (key, gas) in gas_fields {
            match gas {
                Some(gas) => frame.serialize_field(key, &U64::from(gas))?,
                None => frame.skip_field(key)?,
            }
        }
        frame.serialize_field("returnValue", &self.return_value)?;
        frame.serialize_field("structLogs", &self.struct_logs)?;
        frame.end()
    }
}

/// The step between its `step` and `step_end` hooks, unless a message it started is running.
#[derive(Clone, Copy, Debug, Default)]
enum StepState {
    /// No step is running, or the running step started a message and is an [`OpenStep`].
    #[default]
    None,
    /// The step is not recorded.
    Skipped,
    /// The struct log of the step is written once its gas cost is known.
    Recording(RecordingStep),
}

/// What a struct log reports of a step when the step starts.
#[derive(Clone, Copy, Debug)]
struct RecordingStep {
    pc: u64,
    op: OpCode,
    /// The remaining gas.
    gas: u64,
    /// The remaining gas plus spilled state gas.
    gas_before: u64,
    /// What this step had charged when it started its first message.
    message_charge: Option<MessageCharge>,
    depth: u64,
    refund: u64,
    state_gas_spent: i64,
    /// The state gas reservoir, if the fork has one.
    state_gas_reservoir: Option<u64>,
    /// The step's known JSON bytes already charged to the recording budget.
    reserved: usize,
}

impl RecordingStep {
    /// Returns the net state gas the step has spent, given the state gas spent since it started,
    /// if the fork tracks state gas and the step changed it.
    const fn state_gas_cost(&self, state_gas_spent: i64) -> Option<i64> {
        let cost = state_gas_spent.saturating_sub(self.state_gas_spent);
        if self.state_gas_reservoir.is_some() && cost != 0 { Some(cost) } else { None }
    }

    /// Returns the state gas reservoir the struct log reports, which omits an empty one like a
    /// typed struct log.
    fn reported_reservoir(&self) -> Option<u64> {
        self.state_gas_reservoir.filter(|reservoir| *reservoir != 0)
    }

    fn write_cost_and_depth(
        &self,
        head: &mut ArrayBuf<HEAD_CAPACITY>,
        cost: u64,
        state_cost: Option<i64>,
    ) {
        head.u64(cost);
        if let Some(cost) = state_cost {
            head.bytes(STATE_GAS_COST_KEY);
            head.i64(cost);
        }
        if let Some(reservoir) = self.reported_reservoir() {
            head.bytes(b",\"stateGasReservoir\":");
            head.u64(reservoir);
        }
        head.bytes(b",\"depth\":");
        head.u64(self.depth);
    }
}

/// What a step had charged when the first message it started began.
#[derive(Clone, Copy, Debug)]
struct MessageCharge {
    cost: u64,
    /// The state gas cost, if the fork tracks state gas and the step changed it.
    state_cost: Option<i64>,
}

/// A step that started a message, until its `step_end` hook.
#[derive(Clone, Debug)]
struct OpenStep {
    step: RecordingStep,
    charge: MessageCharge,
    /// The provisional gas and depth fields in the output.
    fields: Range<usize>,
}

/// The storage slot an `SLOAD` or `SSTORE` step accesses.
#[derive(Clone, Copy, Debug)]
struct StorageAccess {
    /// The contract whose storage is accessed.
    address: Address,
    key: U256,
    /// The value read or written by the step.
    value: U256,
}

/// The fields of a struct log that are copied when its step starts: the stack, the return data
/// and the memory.
///
/// The buffer keeps its initialized bytes across steps, so that a step writes into them without
/// growing or zeroing the buffer.
#[derive(Clone, Debug, Default)]
struct Tail {
    buf: Vec<u8>,
    /// The number of bytes written for the running step.
    len: usize,
}

impl Tail {
    const fn clear(&mut self) {
        self.len = 0;
    }

    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Returns `len` writable bytes after the written ones.
    fn spare(&mut self, len: usize) -> &mut [u8] {
        let end = self.len + len;
        if self.buf.len() < end {
            self.buf.resize(end, 0);
        }
        &mut self.buf[self.len..end]
    }

    fn push(&mut self, bytes: &[u8]) {
        self.spare(bytes.len()).copy_from_slice(bytes);
        self.len += bytes.len();
    }

    fn push_stack(&mut self, stack: &[U256]) {
        const KEY: &[u8] = b",\"stack\":[";
        let dst = self.spare(KEY.len() + stack.len() * (WORD_JSON_LEN + 1) + 1);
        dst[..KEY.len()].copy_from_slice(KEY);
        let mut len = KEY.len();
        for (index, word) in stack.iter().enumerate() {
            if index != 0 {
                dst[len] = b',';
                len += 1;
            }
            len += write_word(&mut dst[len..], word);
        }
        dst[len] = b']';
        self.len += len + 1;
    }

    /// Returns the bytes [`Self::push_return_data`] writes for `data`.
    const fn return_data_len(data: &[u8]) -> usize {
        RETURN_DATA_KEY.len() + 2 + data.len() * 2 + 1
    }

    fn push_return_data(&mut self, data: &[u8]) {
        self.push(RETURN_DATA_KEY);
        self.push(hex::encode_prefixed(data).as_bytes());
        self.push(b"\"");
    }

    /// Returns the bytes [`Self::push_memory`] writes for `memory`, which must not be empty.
    const fn memory_len(memory: &[u8]) -> usize {
        MEMORY_KEY.len() + memory.len().div_ceil(32) * (WORD_JSON_LEN + 1)
    }

    /// Writes `memory`, which must not be empty, as 32-byte chunks, the last one padded with
    /// zeros.
    fn push_memory(&mut self, memory: &[u8]) {
        self.push(MEMORY_KEY);
        let (chunks, rest) = memory.as_chunks::<32>();
        let last = (!rest.is_empty()).then(|| {
            let mut last = [0; 32];
            last[..rest.len()].copy_from_slice(rest);
            last
        });
        let chunks = chunks.iter().chain(&last);
        let len = memory.len().div_ceil(32) * (WORD_JSON_LEN + 1);
        let (dst, _) = self.spare(len).as_chunks_mut::<{ WORD_JSON_LEN + 1 }>();
        for (chunk, dst) in chunks.zip(dst) {
            dst[0] = b'"';
            dst[1..67].copy_from_slice(hex::Buffer::<32, true>::new().format(chunk).as_bytes());
            dst[67] = b'"';
            dst[68] = b',';
        }
        self.len += len;
        // The separator after the last chunk closes the array instead.
        self.buf[self.len - 1] = b']';
    }
}

/// The fields of struct logs that were only known after later struct logs were written: the state
/// gas cost and the error of a step that started a message.
#[derive(Clone, Debug, Default)]
struct Patches {
    /// The range replaced in the struct logs and the range of its replacement in `bytes`.
    entries: Vec<(Range<usize>, Range<usize>)>,
    bytes: Vec<u8>,
}

impl Patches {
    fn clear(&mut self) {
        self.entries.clear();
        self.bytes.clear();
    }

    /// Replaces a range with its completed fields and returns the additional byte count.
    fn replace(&mut self, fields: Range<usize>, write: impl FnOnce(&mut Vec<u8>)) -> usize {
        let start = self.bytes.len();
        write(&mut self.bytes);
        let replacement = start..self.bytes.len();
        // Setting a sentry's cost to zero can remove digits, but its error field adds more.
        let added = replacement.len() - fields.len();
        self.entries.push((fields, replacement));
        added
    }

    /// Inserts the patches into `logs` and clears them.
    ///
    /// This moves each byte of `logs` at most once, however many patches there are.
    fn apply(&mut self, logs: &mut Vec<u8>) {
        // A step that started a message ends after the steps of the message, so the patches are
        // not in the order of their offsets.
        self.entries.sort_unstable_by_key(|(fields, _)| fields.start);
        let mut src_end = logs.len();
        let mut dst_end = src_end
            + self.entries.iter().map(|(fields, patch)| patch.len() - fields.len()).sum::<usize>();
        logs.resize(dst_end, 0);
        for (fields, patch) in self.entries.iter().rev() {
            let moved = src_end - fields.end;
            logs.copy_within(fields.end..src_end, dst_end - moved);
            dst_end -= moved;
            let patch = &self.bytes[patch.clone()];
            dst_end -= patch.len();
            logs[dst_end..dst_end + patch.len()].copy_from_slice(patch);
            src_end = fields.start;
        }
        debug_assert_eq!(src_end, dst_end);
        self.clear();
    }
}

/// A byte buffer on the stack, in which the short fields of a struct log are assembled before
/// they are appended at once.
struct ArrayBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> ArrayBuf<N> {
    const fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }

    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    /// Appends the first `len` bytes of `bytes`.
    ///
    /// This copies all of `bytes`, a fixed number of them, which is cheaper than copying a few
    /// bytes of a length that is only known at runtime.
    fn padded<const M: usize>(&mut self, bytes: &[u8; M], len: usize) {
        debug_assert!(len <= M);
        self.buf[self.len..self.len + M].copy_from_slice(bytes);
        self.len += len;
    }

    /// Appends the written bytes to `out`.
    fn append_to(&self, out: &mut Vec<u8>) {
        // Short contents are appended with a copy of a fixed number of bytes, see `padded`.
        const FIXED: usize = 128;
        if N >= FIXED && self.len <= FIXED {
            let len = out.len() + self.len;
            out.extend_from_slice(&self.buf[..FIXED]);
            out.truncate(len);
        } else {
            out.extend_from_slice(self.as_slice());
        }
    }

    /// Appends the decimal digits of `value`, which needs room for 20 bytes.
    fn u64(&mut self, mut value: u64) {
        // The digits are written from the least significant one, up to the end of the first 20
        // bytes. The padding after them lets a fixed number of bytes be copied, see `padded`.
        let mut digits = [0; 40];
        let mut start = 20;
        while value >= 100 {
            let pair = (value % 100) as usize * 2;
            value /= 100;
            start -= 2;
            digits[start..start + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
        }
        if value >= 10 {
            let pair = value as usize * 2;
            start -= 2;
            digits[start..start + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
        } else {
            start -= 1;
            digits[start] = b'0' + value as u8;
        }
        self.buf[self.len..self.len + 20].copy_from_slice(&digits[start..][..20]);
        self.len += 20 - start;
    }

    /// Appends the decimal digits of `value`, after its sign if it is negative.
    fn i64(&mut self, value: i64) {
        if value < 0 {
            self.bytes(b"-");
        }
        self.u64(value.unsigned_abs());
    }
}

/// The bytes the fields that [`RawStructLogInspector::write_log`] assembles on the stack take at
/// most: 199 for the separator, the keys and six numbers of up to 20 digits up to the depth, an
/// opcode name and 31 for the refund and the closing brace.
const HEAD_CAPACITY: usize = 199 + OPCODE_NAME_CAPACITY + 31;

/// The bytes the name of an opcode takes at most: those of the string an opcode that is not
/// defined is reported with, which is longer than every name.
const OPCODE_NAME_CAPACITY: usize = "opcode 0xff not defined".len();

/// The name of every opcode with its length, padded so that copying a name copies a fixed number
/// of bytes.
const OPCODE_NAMES: [([u8; OPCODE_NAME_CAPACITY], u8); 256] = {
    let mut names = [([0; OPCODE_NAME_CAPACITY], 0); 256];
    let mut opcode = 0;
    while opcode < names.len() {
        let name = OpCode::new_or_unknown(opcode as u8).as_str().as_bytes();
        assert!(name.len() <= OPCODE_NAME_CAPACITY);
        let mut index = 0;
        while index < name.len() {
            // The names are written into the JSON without escaping.
            assert!(name[index].is_ascii_alphanumeric());
            names[opcode].0[index] = name[index];
            index += 1;
        }
        names[opcode].1 = name.len() as u8;
        opcode += 1;
    }
    names
};

/// The most bytes the JSON of a 32-byte word takes: the quotes, the `0x` prefix and 64 digits.
const WORD_JSON_LEN: usize = 2 + 2 + 64;

/// The bytes of a storage slot in the JSON of a struct log, with the brace or the separator
/// before it: the quoted key, the colon and the quoted value.
const STORAGE_ENTRY_LEN: usize = WORD_JSON_LEN + 1 + WORD_JSON_LEN;

const STATE_GAS_COST_KEY: &[u8] = b",\"stateGasCost\":";

const ERROR_KEY: &[u8] = b",\"error\":";

/// A parent can gain a signed state gas cost and an error. Its gas cost can only shrink.
/// `CreateContractStartingWithEF` is the longest halt name and `i64::MIN` takes 20 bytes.
const MAX_PATCH_GROWTH: u64 = (STATE_GAS_COST_KEY.len()
    + 20
    + ERROR_KEY.len()
    + br#""Some(CreateContractStartingWithEF)""#.len()) as u64;

const RETURN_DATA_KEY: &[u8] = b",\"returnData\":\"";

const MEMORY_KEY: &[u8] = b",\"memory\":[";

/// The key of the storage. The braces of its object are written with the slots.
const STORAGE_KEY: &[u8] = b",\"storage\":";

/// The decimal digits of every number below 100, two for each.
const DIGIT_PAIRS: &[u8; 200] = b"\
    00010203040506070809101112131415161718192021222324\
    25262728293031323334353637383940414243444546474849\
    50515253545556575859606162636465666768697071727374\
    75767778798081828384858687888990919293949596979899";

/// Returns the JSON string of the error a struct log reports for a step that halted with `status`.
#[cold]
fn error_json(status: InstrStop) -> Option<String> {
    serde_json::to_string(&format!("{:?}", Some(status))).ok()
}

/// Writes the quoted `0x` prefixed hex of `word` without leading zeros at the start of `dst`,
/// which must hold [`WORD_JSON_LEN`] bytes, and returns the number of bytes written.
fn write_word(dst: &mut [u8], word: &U256) -> usize {
    // All 64 digits, followed by padding so that the digits without the leading zeros are copied
    // with a fixed number of bytes, see `ArrayBuf::padded`.
    let mut digits = [0; 128];
    digits[..64].copy_from_slice(
        hex::Buffer::<32, false>::new().format(&word.to_be_bytes::<32>()).as_bytes(),
    );
    let len = word.bit_len().div_ceil(4).max(1);
    let dst = &mut dst[..WORD_JSON_LEN];
    dst[..3].copy_from_slice(b"\"0x");
    dst[3..67].copy_from_slice(&digits[64 - len..][..64]);
    dst[3 + len] = b'"';
    len + 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::utils::convert_memory;
    use alloc::string::ToString;

    #[test]
    fn patches_are_inserted_in_the_order_of_their_offsets() {
        let mut logs = b"0123456789".to_vec();
        let mut patches = Patches::default();
        patches.apply(&mut logs);
        assert_eq!(logs, b"0123456789");

        // Patches arrive innermost call first, which is not the order of their offsets.
        for (offset, patch) in [(7, "c"), (4, "bb"), (10, "d"), (0, "aaa")] {
            assert_eq!(
                patches.replace(offset..offset, |bytes| bytes.extend_from_slice(patch.as_bytes())),
                patch.len()
            );
        }
        patches.apply(&mut logs);
        assert_eq!(logs, b"aaa0123bb456c789d");
        assert!(patches.entries.is_empty() && patches.bytes.is_empty());
    }

    #[test]
    fn numbers_are_written_in_decimal() {
        let mut values = alloc::vec![0, u64::MAX];
        for digits in 0..20 {
            let power = 10u64.pow(digits);
            values.extend([power - 1, power, power + 1, power.saturating_mul(9)]);
        }
        for value in values {
            let mut buf = ArrayBuf::<24>::new();
            buf.u64(value);
            assert_eq!(buf.as_slice(), value.to_string().as_bytes());
        }
        for value in [0, 1, -1, 97_920, -97_920, i64::MAX, i64::MIN] {
            let mut buf = ArrayBuf::<24>::new();
            buf.i64(value);
            assert_eq!(buf.as_slice(), value.to_string().as_bytes());
        }
    }

    #[test]
    fn words_are_written_as_minimal_hex() {
        let check = |word: U256| {
            let mut buf = [0; WORD_JSON_LEN];
            let len = write_word(&mut buf, &word);
            assert_eq!(&buf[..len], format!("\"{word:#x}\"").as_bytes());
        };
        check(U256::ZERO);
        // Every digit count, with the top digit both minimal and maximal.
        for shift in 0..256 {
            check(U256::from(1) << shift);
            check(U256::MAX >> shift);
        }
    }

    #[test]
    fn buffers_are_written_like_the_typed_struct_logs() {
        // The last chunk of memory is padded with zeros.
        for len in [1, 31, 32, 33, 64, 100] {
            let memory = (0..len).map(|byte| byte as u8).collect::<Vec<_>>();
            let chunks = serde_json::to_string(&convert_memory(&memory)).unwrap();
            let mut tail = Tail::default();
            tail.push_memory(&memory);
            assert_eq!(tail.as_slice(), format!(",\"memory\":{chunks}").as_bytes());
            assert_eq!(tail.len, Tail::memory_len(&memory));

            tail.clear();
            tail.push_return_data(&memory);
            let data = serde_json::to_string(&Bytes::from(memory.clone())).unwrap();
            assert_eq!(tail.as_slice(), format!(",\"returnData\":{data}").as_bytes());
            assert_eq!(tail.len, Tail::return_data_len(&memory));
        }
    }

    #[test]
    fn fuse_discards_initialized_tail_bytes() {
        let mut inspector = RawStructLogInspector::new(Default::default());
        inspector.tail.push_return_data(b"previous transaction");
        inspector.fuse();
        assert!(inspector.tail.buf.is_empty());
        assert_eq!(inspector.tail.len, 0);
    }
}
