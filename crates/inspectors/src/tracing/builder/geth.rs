//! Geth trace builder
use crate::tracing::types::{CallKind, CallTraceNode, CallTraceStepStackItem, StorageChange};
use alloc::{borrow::Cow, boxed::Box, collections::BTreeMap, vec, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, U256, map::HashMap};
use alloy_rpc_types_trace::geth::{
    CallConfig, CallFrame, DefaultFrame, GethDefaultTracingOptions, StructLog,
    erc7562::CallFrameType,
};
use evm2::{EvmFeatures, TxResultExt};

/// The smallest size in bytes of the JSON of a struct log,
/// `{"pc":0,"op":"OR","gas":0,"gasCost":0,"depth":1}`.
pub(crate) const MIN_STRUCT_LOG_JSON_LEN: u64 = 48;

/// Returns the size in bytes of the JSON that geth's struct logger writes for the log.
///
/// geth counts these bytes against the `limit` option. The JSON is compact, orders the fields as
/// `pc`, `op`, `gas`, `gasCost`, `depth`, `error`, `stack`, `returnData`, `memory`, `storage` and
/// `refund`, and omits the optional ones that are unset, empty or zero. It also counts the
/// `stateGasCost` and `stateGasReservoir` fields that evm2 adds after EIP-8037. It escapes `<`, `>`
/// and `&` in strings as `\u00xx`.
///
/// See <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/eth/tracers/logger/logger.go#L145-L155>
fn struct_log_json_len(log: &StructLog) -> usize {
    const fn digits(value: u64) -> usize {
        match value.checked_ilog10() {
            Some(log) => log as usize + 1,
            None => 1,
        }
    }
    // The quoted `0x`-prefixed minimal hex of a word.
    fn word(value: &U256) -> usize {
        2 + 2 + value.bit_len().div_ceil(4).max(1)
    }
    // The quoted `0x`-prefixed hex of a 32-byte hash.
    const HASH: usize = 2 + 2 + 64;

    // `{"pc":`, `,"op":"`, `","gas":`, `,"gasCost":`, `,"depth":` and `}`.
    let mut len = 6 + digits(log.pc) + 7 + log.op.len() + 1 + 7 + digits(log.gas);
    len += 11 + digits(log.gas_cost) + 9 + digits(log.depth) + 1;
    // The fields after EIP-8037 that evm2 adds to the struct log: `,"stateGasCost":` and
    // `,"stateGasReservoir":`.
    if let Some(cost) = log.state_gas_cost {
        len += 16 + usize::from(cost < 0) + digits(cost.unsigned_abs());
    }
    if let Some(reservoir) = log.state_gas_reservoir {
        len += 21 + digits(reservoir);
    }
    if let Some(error) = log.error.as_ref().filter(|error| !error.is_empty()) {
        let escaped = error.bytes().filter(|byte| matches!(byte, b'<' | b'>' | b'&')).count();
        len += 10 + error.len() + 5 * escaped + 1;
    }
    if let Some(stack) = &log.stack {
        len += 10 + stack.iter().map(word).sum::<usize>() + stack.len().saturating_sub(1) + 1;
    }
    if let Some(return_data) = log.return_data.as_ref().filter(|data| !data.is_empty()) {
        len += 15 + 2 + 2 * return_data.len() + 1;
    }
    if let Some(memory) = log.memory.as_ref().filter(|memory| !memory.is_empty()) {
        len += 11 + memory.iter().map(|chunk| chunk.len() + 2).sum::<usize>();
        len += memory.len().saturating_sub(1) + 1;
    }
    if let Some(storage) = log.storage.as_ref().filter(|storage| !storage.is_empty()) {
        len += 12 + storage.len() * (HASH + 1 + HASH) + storage.len().saturating_sub(1) + 1;
    }
    if let Some(refund) = log.refund_counter.filter(|refund| *refund != 0) {
        len += 10 + digits(refund);
    }
    len
}

/// A frame awaiting tree assembly, with visibility inherited by children in the forward pass.
struct PendingCallFrame {
    /// Whether this call and its ancestors permit descendant logs. Independent of whether
    /// the frame itself contains logs.
    logs_visible: bool,
    frame: CallFrame,
}

#[cfg(test)]
mod steps_tests;

/// The stack snapshots taken from one node's steps, in step order.
pub(crate) type StackSnapshots = vec::IntoIter<Option<Box<[U256]>>>;

/// A type for creating geth style traces
#[derive(Clone, Debug)]
pub struct GethTraceBuilder<'a> {
    /// Recorded trace nodes.
    nodes: Cow<'a, [CallTraceNode]>,
    /// EVM features used to produce the trace.
    features: EvmFeatures,
}

impl GethTraceBuilder<'static> {
    /// Returns a new instance of the builder from [`Cow::Owned`]
    pub const fn new(nodes: Vec<CallTraceNode>) -> Self {
        Self { nodes: Cow::Owned(nodes), features: EvmFeatures::empty() }
    }
}

impl<'a> GethTraceBuilder<'a> {
    /// Returns a new instance of the builder from [`Cow::Borrowed`]
    pub const fn new_borrowed(nodes: &'a [CallTraceNode]) -> Self {
        Self { nodes: Cow::Borrowed(nodes), features: EvmFeatures::empty() }
    }

    /// Sets the EVM features used to produce the trace.
    pub const fn with_features(mut self, features: EvmFeatures) -> Self {
        self.features = features;
        self
    }

    /// Consumes the builder and returns the recorded trace nodes.
    pub fn to_owned(self) -> Vec<CallTraceNode> {
        self.nodes.into_owned()
    }

    /// Returns the sum of all steps in the recorded node traces.
    fn trace_step_count(&self) -> usize {
        self.nodes.iter().map(|node| node.trace.steps.len()).sum()
    }

    #[inline]
    const fn is_eip8037_enabled(&self) -> bool {
        self.features.contains(EvmFeatures::EIP8037)
    }

    /// Fill in the geth trace with all steps of the trace and its children traces in the order they
    /// appear in the transaction.
    fn fill_geth_trace(
        &self,
        main_trace_node: &CallTraceNode,
        opts: &GethDefaultTracingOptions,
        storage: &mut HashMap<Address, BTreeMap<B256, B256>>,
        struct_logs: &mut Vec<StructLog>,
        mut stacks: Option<&mut [StackSnapshots]>,
    ) {
        // Suspend only the parent iterator when entering a child. Scratch space scales with
        // call depth rather than step count, and a trace without subcalls needs no allocation.
        let mut steps = main_trace_node.steps_with_children();
        let mut parents = Vec::new();
        let mut json_len = 0u64;

        loop {
            // geth stops logging once the JSON written so far is over the limit.
            if opts.limit.is_some_and(|limit| json_len > limit) {
                break;
            }
            let Some(CallTraceStepStackItem { trace_node, step, call_child_id }) = steps.next()
            else {
                let Some(parent) = parents.pop() else { break };
                steps = parent;
                continue;
            };

            // We increment the depth by one because steps that are part of call at depth N should
            // have depth N + 1. For example, steps inside of a top-level call should
            // have depth 1.
            let stack = if !opts.is_stack_enabled() {
                None
            } else if let Some(stacks) = stacks.as_deref_mut() {
                stacks[trace_node.idx].next().flatten().map(Vec::from)
            } else {
                step.stack.as_deref().map(<[U256]>::to_vec)
            };
            let mut log =
                step.convert_to_geth_struct_log(opts, trace_node.trace.depth as u64 + 1, stack);

            // geth snapshots the storage of the contract, with the slots of all its earlier
            // SLOADs and SSTOREs, unless the opcode fails before it runs.
            if opts.is_storage_enabled()
                && !step.is_error()
                && let Some(StorageChange { key, value, .. }) =
                    step.storage_change.as_deref().copied()
            {
                let contract_storage = storage.entry(trace_node.execution_address()).or_default();
                contract_storage.insert(key.into(), value.into());
                log.storage = Some(contract_storage.clone());
            }

            if opts.is_return_data_enabled() && !step.returndata.is_empty() {
                log.return_data = Some(step.returndata.clone());
            }

            // Add step to geth trace
            if opts.limit.is_some() {
                json_len = json_len.saturating_add(struct_log_json_len(&log) as u64);
            }
            struct_logs.push(log);

            // Visit the child immediately after its call opcode, then resume the parent.
            if let Some(call_child_id) = call_child_id {
                parents.push(steps);
                steps = self.nodes[call_child_id].steps_with_children();
            }
        }
    }

    /// Generate a geth-style trace e.g. for `debug_traceTransaction`
    ///
    /// This expects the gas used and return value for the executed transaction.
    pub fn geth_traces(
        &self,
        receipt_gas_used: u64,
        return_value: Bytes,
        opts: GethDefaultTracingOptions,
    ) -> DefaultFrame {
        self.geth_traces_inner(receipt_gas_used, None, return_value, opts, None)
    }

    /// Generate a geth-style trace with EIP-8037 transaction gas accounting.
    pub fn geth_traces_with_result_gas<E>(
        &self,
        gas: &TxResultExt<E>,
        return_value: Bytes,
        opts: GethDefaultTracingOptions,
    ) -> DefaultFrame {
        self.geth_traces_inner(
            gas.tx_gas_used(),
            Some((gas.execution_gas_spent(), gas.state_gas_spent(), gas.final_refunded())),
            return_value,
            opts,
            None,
        )
    }

    /// Generate a geth-style trace.
    ///
    /// If `stacks` is set, the struct log stacks are taken from it instead of copied from the
    /// steps. It holds the stack snapshots of each node's steps in step order, indexed by node.
    pub(crate) fn geth_traces_inner(
        &self,
        receipt_gas_used: u64,
        gas: Option<(u64, u64, u64)>,
        return_value: Bytes,
        opts: GethDefaultTracingOptions,
        stacks: Option<&mut [StackSnapshots]>,
    ) -> DefaultFrame {
        if self.nodes.is_empty() {
            return Default::default();
        }
        // Fetch top-level trace
        let main_trace_node = &self.nodes[0];
        let main_trace = &main_trace_node.trace;

        let opts =
            GethDefaultTracingOptions { limit: opts.limit.filter(|limit| *limit != 0), ..opts };
        let capacity = opts.limit.map_or_else(
            || self.trace_step_count(),
            |limit| {
                let steps = limit / MIN_STRUCT_LOG_JSON_LEN + 1;
                self.trace_step_count().min(usize::try_from(steps).unwrap_or(usize::MAX))
            },
        );
        let mut struct_logs = Vec::with_capacity(capacity);
        let mut storage = HashMap::default();
        self.fill_geth_trace(main_trace_node, &opts, &mut storage, &mut struct_logs, stacks);

        let gas = gas.filter(|_| self.is_eip8037_enabled());

        DefaultFrame {
            // If the top-level trace succeeded, then it was a success
            failed: !main_trace.success,
            gas: receipt_gas_used,
            execution_gas_used: gas.map(|gas| gas.0),
            state_gas_used: gas.map(|gas| gas.1),
            gas_refund: gas.map(|gas| gas.2),
            return_value,
            struct_logs,
        }
    }

    /// Generate a geth-style traces for the call tracer.
    ///
    /// This decodes all call frames from the recorded traces.
    ///
    /// This expects the gas used and return value for the executed transaction.
    pub fn geth_call_traces(&self, opts: CallConfig, gas_used: u64) -> CallFrame {
        self.geth_call_traces_inner(opts, gas_used, None)
    }

    /// Generate a geth-style call trace with EIP-8037 transaction gas accounting.
    pub fn geth_call_traces_with_result_gas<E>(
        &self,
        opts: CallConfig,
        gas: &TxResultExt<E>,
    ) -> CallFrame {
        self.geth_call_traces_inner(
            opts,
            gas.tx_gas_used(),
            Some((gas.execution_gas_spent(), gas.state_gas_spent(), gas.final_refunded())),
        )
    }

    fn geth_call_traces_inner(
        &self,
        opts: CallConfig,
        gas_used: u64,
        gas: Option<(u64, u64, u64)>,
    ) -> CallFrame {
        if self.nodes.is_empty() {
            return Default::default();
        }

        let include_logs = opts.with_log.unwrap_or_default();
        // first fill up the root
        let main_trace_node = &self.nodes[0];
        // Like any other failed call, a failed root drops its own logs.
        let root_logs_visible = include_logs && !main_trace_node.trace.is_error();
        let mut root_call_frame = main_trace_node.geth_empty_call_frame(root_logs_visible);
        root_call_frame.gas_used = U256::from(gas_used);
        if let Some(gas) = gas.filter(|_| self.is_eip8037_enabled()) {
            root_call_frame.execution_gas_used = Some(U256::from(gas.0));
            root_call_frame.state_gas_used = Some(U256::from(gas.1));
            root_call_frame.gas_refund = Some(U256::from(gas.2));
        }

        if opts.only_top_call.unwrap_or_default() {
            // No subcall frames are collected, so logs have no position relative to them.
            for log in &mut root_call_frame.logs {
                log.position = Some(0);
            }
            return root_call_frame;
        }

        // selfdestructs are not recorded as individual call traces but are derived from
        // the call trace and are added as additional `CallFrame` objects to the parent call
        if let Some(selfdestruct) = main_trace_node.geth_selfdestruct_call_trace() {
            root_call_frame.calls.push(selfdestruct);
        }

        // fill all the call frames in the root call frame with the recorded traces.
        // traces are identified by their index in the arena
        // so we can populate the call frame tree by walking up the call tree
        let mut call_frames = Vec::with_capacity(self.nodes.len());
        call_frames
            .push(PendingCallFrame { logs_visible: root_logs_visible, frame: root_call_frame });

        for trace in self.nodes.iter().skip(1) {
            // Parents precede their children, so inherit log visibility without walking back
            // to the root for every call. Keep the flag alongside the frame we already allocate.
            let include_logs = include_logs
                && !trace.trace.is_error()
                && trace.parent.is_none_or(|parent| call_frames[parent].logs_visible);
            call_frames.push(PendingCallFrame {
                logs_visible: include_logs,
                frame: trace.geth_empty_call_frame(include_logs),
            });

            // selfdestructs are not recorded as individual call traces but are derived from
            // the call trace and are added as additional `CallFrame` objects
            // becoming the first child of the derived call
            if let Some(selfdestruct) = trace.geth_selfdestruct_call_trace() {
                call_frames.last_mut().expect("not empty").frame.calls.push(selfdestruct);
            }
        }

        // pop the _children_ calls frame and move it to the parent
        // this will roll up the child frames to their parent; this works because `child idx >
        // parent idx`
        loop {
            let PendingCallFrame { frame: mut call, .. } =
                call_frames.pop().expect("call frames not empty");
            let idx = call_frames.len();
            // Children are appended in reverse call order because we walk the arena from the
            // back, and the selfdestruct frame (if any) was pushed first. Reversing once per frame
            // restores call order and moves the selfdestruct frame last.
            call.calls.reverse();
            let node = &self.nodes[idx];
            if let Some(parent) = node.parent {
                call_frames[parent].frame.calls.push(call);
            } else {
                debug_assert!(call_frames.is_empty(), "only one root node has no parent");
                return call;
            }
        }
    }

    /// Converts a CallKind to a CallFrameType.
    pub const fn convert_call_kind(kind: CallKind) -> CallFrameType {
        match kind {
            CallKind::Call => CallFrameType::Call,
            CallKind::CallCode => CallFrameType::CallCode,
            CallKind::DelegateCall => CallFrameType::DelegateCall,
            CallKind::StaticCall => CallFrameType::StaticCall,
            CallKind::Create => CallFrameType::Create,
            CallKind::Create2 => CallFrameType::Create2,
            CallKind::AuthCall => CallFrameType::Call,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::types::{CallLog, CallTrace};
    use evm2::interpreter::InstrStop;

    #[test]
    fn log_visibility_follows_ancestors_and_preserves_siblings() {
        // The reverted branch has a log-free intermediate call. Visibility must propagate
        // through it, while the later successful sibling and its children keep their logs.
        let parents = [None, Some(0), Some(1), Some(2), Some(0), Some(0), Some(5)];
        for root_failed in [false, true] {
            let mut nodes: Vec<CallTraceNode> = parents
                .iter()
                .enumerate()
                .map(|(idx, &parent)| {
                    let failed = idx == 1 || idx == 4 || (idx == 0 && root_failed);
                    CallTraceNode {
                        idx,
                        parent,
                        trace: CallTrace {
                            address: Address::with_last_byte(idx as u8),
                            success: !failed,
                            status: Some(if idx == 4 {
                                InstrStop::OutOfGas
                            } else if failed {
                                InstrStop::Revert
                            } else {
                                InstrStop::Return
                            }),
                            ..Default::default()
                        },
                        logs: if idx == 2 {
                            vec![]
                        } else {
                            [0, 1]
                                .into_iter()
                                .map(|log| {
                                    CallLog::from(alloy_primitives::Log::new_unchecked(
                                        Address::with_last_byte(idx as u8),
                                        vec![B256::with_last_byte(log)],
                                        Bytes::from(vec![idx as u8, log]),
                                    ))
                                    .with_index((idx * 2 + log as usize) as u64)
                                })
                                .collect()
                        },
                        ..Default::default()
                    }
                })
                .collect();
            for (idx, parent) in parents.into_iter().enumerate() {
                if let Some(parent) = parent {
                    nodes[parent].children.push(idx);
                }
            }
            let builder = GethTraceBuilder::new(nodes);
            for with_log in [false, true] {
                let call = builder.geth_call_traces(
                    CallConfig { with_log: Some(with_log), ..Default::default() },
                    0,
                );
                let mut pending = vec![&call];
                let mut visited = Vec::new();
                while let Some(call) = pending.pop() {
                    let idx = call.to.unwrap().as_slice()[19] as usize;
                    visited.push(idx);
                    let visible = with_log && !root_failed && (idx == 0 || idx >= 5);
                    assert_eq!(call.logs.len(), if visible { 2 } else { 0 }, "node {idx}");
                    if visible {
                        for (log_idx, log) in call.logs.iter().enumerate() {
                            assert_eq!(log.index, Some((idx * 2 + log_idx) as u64));
                            assert_eq!(log.topics, Some(vec![B256::with_last_byte(log_idx as u8)]));
                            assert_eq!(log.data, Some(Bytes::from(vec![idx as u8, log_idx as u8])));
                        }
                    }
                    pending.extend(call.calls.iter().rev());
                }
                assert_eq!(visited, [0, 1, 2, 3, 4, 5, 6]);
            }
        }
    }
}
