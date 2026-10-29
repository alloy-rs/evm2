//! Tracer selection and trace rendering on top of `evm2-inspectors`.

use super::{Args, Error, output::TxTrace};
use alloy_consensus::Transaction;
use alloy_primitives::{B256, hex};
use alloy_rpc_types_trace::geth::{
    CallConfig, CallFrame, GethDebugTracingOptions, GethDefaultTracingOptions, StructLog,
};
use evm2::{
    BaseEvmTypes, TxResultWithState,
    env::BlockEnv,
    ethereum::RecoveredTxEnvelope,
    evm::DynDatabase,
    interpreter::{op, opcode::OpCode},
};
use evm2_inspectors::tracing::{DebugInspector, types::CallTraceNode};
use serde_json::{Value, json};
use std::{borrow::Cow, collections::HashMap};

/// How the execution is traced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    /// EIP-3155 JSON lines, written to `trace-<index>-<hash>.jsonl`.
    Jsonl,
    /// The default struct logger frame, as `debug_traceTransaction` returns it.
    StructLog,
    /// A geth tracer, selected by name or JavaScript source.
    Tracer,
}

/// The tracing configuration for a run.
#[derive(Clone, Debug, Default)]
pub struct Tracing {
    kind: Option<TraceKind>,
    options: GethDebugTracingOptions,
    /// Whether the JSON lines carry the memory; the logger always records it for `memSize`.
    memory: bool,
}

impl Tracing {
    /// No tracing.
    pub fn none() -> Self {
        Self::default()
    }

    /// EIP-3155 lines, or the default struct logger frame when `structlog` is set.
    pub fn struct_logger(kind: TraceKind, memory: bool, no_stack: bool, return_data: bool) -> Self {
        let config = GethDefaultTracingOptions {
            enable_memory: Some(memory || kind == TraceKind::Jsonl),
            disable_stack: Some(no_stack),
            disable_storage: Some(kind == TraceKind::Jsonl),
            enable_return_data: Some(return_data),
            ..Default::default()
        };
        let options = GethDebugTracingOptions { config, ..Default::default() };
        Self { kind: Some(kind), options, memory }
    }

    /// A geth tracer by name or JavaScript source, with its JSON configuration.
    pub fn tracer(tracer: &str, config: Option<&str>) -> Result<Self, Error> {
        let mut options = json!({ "tracer": tracer });
        if let Some(config) = config {
            options["tracerConfig"] = serde_json::from_str(config)
                .map_err(|err| Error::Tracer(format!("invalid --trace.jsonconfig: {err}")))?;
        }
        let options =
            serde_json::from_value(options).map_err(|err| Error::Tracer(err.to_string()))?;
        Ok(Self { kind: Some(TraceKind::Tracer), options, memory: false })
    }

    pub(super) fn from_args(args: &Args) -> Result<Self, Error> {
        if let Some(tracer) = &args.tracer {
            return Self::tracer(tracer, args.jsonconfig.as_deref());
        }
        let kind = match (args.trace, args.structlog) {
            (true, _) => TraceKind::Jsonl,
            (_, true) => TraceKind::StructLog,
            _ => return Ok(Self::none()),
        };
        Ok(Self::struct_logger(kind, args.memory, args.nostack, args.returndata))
    }

    pub(super) fn inspector(&self) -> Result<Option<DebugInspector>, Error> {
        self.kind
            .map(|_| DebugInspector::new(self.options.clone()))
            .transpose()
            .map_err(|err| Error::Tracer(err.to_string()))
    }

    /// The empty file `--trace` leaves for a transaction go-ethereum rejects after starting it.
    pub(super) fn empty(&self, index: usize, hash: B256) -> Option<TxTrace> {
        (self.kind == Some(TraceKind::Jsonl)).then(|| TxTrace {
            index,
            hash,
            extension: "jsonl",
            content: String::new(),
        })
    }

    /// Renders the finished transaction's trace.
    pub(super) fn collect(
        &self,
        inspector: &mut DebugInspector,
        (index, hash): (usize, B256),
        tx: &RecoveredTxEnvelope,
        block: &BlockEnv,
        result: &TxResultWithState<BaseEvmTypes>,
        db: &mut dyn DynDatabase,
    ) -> Result<TxTrace, Error> {
        let (extension, content) = if self.kind == Some(TraceKind::Jsonl) {
            ("jsonl", jsonl(inspector, tx, result, self.memory)?)
        } else {
            let trace = inspector
                .get_result(None, tx, block, result, db)
                .map_err(|err| Error::Tracer(err.to_string()))?;
            ("json", serde_json::to_string(&trace).expect("trace serializes"))
        };
        Ok(TxTrace { index, hash, extension, content })
    }
}

/// Renders the recorded call frames as the lines of go-ethereum's JSON logger.
///
/// The logger writes a line for every opcode before it runs, a second line with the error for an
/// opcode that fails while it runs (but not for one that fails its stack or gas checks, which has
/// a single line with the error), and an end line with output, gas used and error for every call
/// frame. The lines come from the steps and frames the inspector recorded.
///
/// The memory sizes and contents come from the recorded steps, which the inspector keeps within
/// its byte budget, not from the struct logs: those hold a hex string per memory word and step.
fn jsonl(
    inspector: &mut DebugInspector,
    tx: &RecoveredTxEnvelope,
    result: &TxResultWithState<BaseEvmTypes>,
    memory: bool,
) -> Result<String, Error> {
    let DebugInspector::Default(tracing, config) = inspector else {
        return Err(Error::Tracer("the JSON logger needs the struct logger".into()));
    };
    if tracing.limit_exceeded() {
        return Err(Error::Tracer("the trace outgrew the inspector's byte budget".into()));
    }
    tracing.set_transaction_gas_limit(tx.gas_limit());
    tracing.set_transaction_caller(tx.signer());
    let options = GethDefaultTracingOptions { enable_memory: Some(false), ..*config };
    let frame = tracing.geth_builder().geth_traces_with_result_gas(
        &result.result,
        result.result.output.clone(),
        options,
    );
    let nodes = tracing.traces().nodes();
    let mut errors = Vec::new();
    collect_errors(&tracing.geth_builder().geth_call_traces(CallConfig::default(), 0), &mut errors);
    let mut writer = JsonlWriter {
        nodes,
        errors,
        logs: frame.struct_logs.iter(),
        opcodes: (0..=u8::MAX).map(|byte| (OpCode::new_or_unknown(byte).as_str(), byte)).collect(),
        // Only EIP-8037 frames report the execution and state gas split.
        amsterdam: frame.execution_gas_used.is_some(),
        memory,
        out: String::new(),
    };
    if !nodes.is_empty() {
        writer.frame(0);
    }
    Ok(writer.out)
}

/// Lists the error of every call frame in the order the frames were entered.
fn collect_errors(frame: &CallFrame, errors: &mut Vec<Option<String>>) {
    // A selfdestruct is a call frame of the geth tree but not a frame the inspector entered.
    if frame.typ != "SELFDESTRUCT" {
        errors.push(frame.error.clone());
    }
    for call in &frame.calls {
        collect_errors(call, errors);
    }
}

struct JsonlWriter<'a> {
    nodes: &'a [CallTraceNode],
    errors: Vec<Option<String>>,
    logs: std::slice::Iter<'a, StructLog>,
    opcodes: HashMap<&'static str, u8>,
    amsterdam: bool,
    memory: bool,
    out: String,
}

impl JsonlWriter<'_> {
    fn line(&mut self, value: Value) {
        self.out.push_str(&value.to_string());
        self.out.push('\n');
    }

    fn frame(&mut self, index: usize) {
        let node = &self.nodes[index];
        let frame_error = self.errors.get(index).cloned().flatten();
        let mut children = node.children.iter();
        let last = node.trace.steps.len().saturating_sub(1);
        for (position, step) in node.trace.steps.iter().enumerate() {
            let Some(log) = self.logs.next() else { break };
            let memory = step.memory.as_ref().map_or(&[][..], |memory| memory.as_bytes());
            let mut line = step_line(log, memory, &self.opcodes, self.memory);
            let op = step.op.get();
            let state_gas_oog = self.amsterdam
                && matches!(op, op::CREATE | op::CREATE2)
                && create_gas(log, op, memory.len()) <= log.gas.into();
            let fault = match &log.error {
                // Stack and gas checks fail before the opcode is logged: one line, with the error.
                Some(error) if fails_before_logging(error, op, state_gas_oog) => {
                    line["error"] = json!(error);
                    None
                }
                Some(error) => Some(error.clone()),
                None if position == last
                    && !matches!(op, op::STOP | op::RETURN | op::SELFDESTRUCT) =>
                {
                    frame_error.clone()
                }
                None => None,
            };
            self.line(line);
            if let Some(error) = fault {
                let out_of_gas = error.contains("OutOfGas");
                let memory = fault_memory(log, op, out_of_gas, memory);
                let mut line =
                    step_line(&fault_log(log, op, out_of_gas), &memory, &self.opcodes, self.memory);
                line["error"] = json!(error);
                self.line(line);
            }
            if op == op::SELFDESTRUCT && node.trace.selfdestruct_address.is_some() {
                // The logger reports a selfdestruct as a call frame that ends at once.
                self.line(json!({ "output": "", "gasUsed": "0x0" }));
            }
            if matches!(
                op,
                op::CALL
                    | op::CALLCODE
                    | op::DELEGATECALL
                    | op::STATICCALL
                    | op::CREATE
                    | op::CREATE2
            ) && let Some(&child) = children.next()
            {
                self.frame(child);
            }
        }
        let mut end = json!({
            "output": hex::encode(&node.trace.output),
            "gasUsed": format!("{:#x}", node.trace.gas_used),
        });
        if let Some(error) = frame_error {
            end["error"] = json!(error);
        }
        self.line(end);
    }
}

/// The byte of an opcode name; an undefined opcode is named `opcode <hex> not defined`.
fn opcode_number(name: &str, opcodes: &HashMap<&str, u8>) -> u8 {
    opcodes.get(name).copied().unwrap_or_else(|| {
        let hex = name.strip_prefix("opcode ").and_then(|rest| rest.split_whitespace().next());
        hex.and_then(|hex| u8::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0xfe)
    })
}

fn step_line(
    log: &StructLog,
    memory: &[u8],
    opcodes: &HashMap<&str, u8>,
    with_memory: bool,
) -> Value {
    let mut line = json!({
        "pc": log.pc,
        "op": opcode_number(&log.op, opcodes),
        "gas": format!("{:#x}", log.gas),
        "gasCost": format!("{:#x}", log.gas_cost),
    });
    line["memSize"] = json!(memory.len());
    if with_memory && !memory.is_empty() {
        line["memory"] = json!(format!("0x{}", hex::encode(memory)));
    }
    line["stack"] = json!(log.stack);
    if let Some(return_data) = &log.return_data
        && !return_data.is_empty()
    {
        line["returnData"] = json!(return_data);
    }
    line["depth"] = json!(log.depth);
    line["refund"] = json!(log.refund_counter.unwrap_or_default());
    line["opName"] = json!(log.op);
    line
}

/// The gas Amsterdam charges a `CREATE` or `CREATE2` before it runs: the base cost, the memory
/// expansion and the initcode words (and the hashed words of `CREATE2`). The state gas of the new
/// account comes after, while the opcode runs. Zero for other opcodes.
fn create_gas(log: &StructLog, op: u8, memory_len: usize) -> u128 {
    const BASE: u128 = 12000;
    let (hashed, Some(stack)) = (op == op::CREATE2, &log.stack) else { return 0 };
    if !matches!(op, op::CREATE | op::CREATE2) || stack.len() < 3 {
        return 0;
    }
    let (offset, size) = (stack[stack.len() - 2], stack[stack.len() - 3]);
    if size.is_zero() {
        return BASE;
    }
    let words = |bytes: usize| bytes.div_ceil(32) as u128;
    let memory_cost = |words: u128| 3 * words + words * words / 512;
    let end = offset.saturating_add(size).saturating_to::<usize>();
    let expansion = memory_cost(words(end)).saturating_sub(memory_cost(words(memory_len)));
    BASE + expansion + words(size.saturating_to::<usize>()) * if hashed { 8 } else { 2 }
}

/// Whether an opcode with this step error fails in its stack or gas checks, before the logger
/// sees it. Static call violations fail in the gas calculation, except for `TSTORE` and `LOG`. From
/// Amsterdam on, a `CREATE` that pays its gas but not its state gas fails while it runs
/// (`state_gas_oog`).
fn fails_before_logging(error: &str, op: u8, state_gas_oog: bool) -> bool {
    let out_of_gas = error.contains("OOG") || error.contains("OutOfGas");
    if out_of_gas {
        return !state_gas_oog;
    }
    error.contains("Stack")
        || error.contains("Static") && !matches!(op, op::TSTORE | op::LOG0..=op::LOG4)
}

/// The memory the JSON logger sees when an opcode fails while it runs: the opcode has grown the
/// memory it addresses.
fn fault_memory<'a>(log: &StructLog, op: u8, out_of_gas: bool, memory: &'a [u8]) -> Cow<'a, [u8]> {
    let Some(stack) = &log.stack else { return Cow::Borrowed(memory) };
    let word = |back: usize| stack.len().checked_sub(back).map(|at| stack[at]);
    // Memory range [offset, offset + size) of the opcodes that fail after growing the memory.
    let range = match op {
        op::REVERT | op::LOG0..=op::LOG4 => word(1).zip(word(2)),
        op::RETURNDATACOPY => word(1).zip(word(3)),
        op::CREATE | op::CREATE2 if out_of_gas => word(2).zip(word(3)),
        _ => None,
    };
    let Some((offset, size)) = range.filter(|(_, size)| !size.is_zero()) else {
        return Cow::Borrowed(memory);
    };
    let words = offset.saturating_add(size).saturating_to::<usize>().div_ceil(32);
    // The opcode only reached its failure after paying for the expansion, so a size the gas
    // cannot cover was never allocated.
    let cost = |words: usize| 3 * words as u128 + (words as u128 * words as u128) / 512;
    if words * 32 <= memory.len()
        || cost(words).saturating_sub(cost(memory.len() / 32)) > u128::from(log.gas)
    {
        return Cow::Borrowed(memory);
    }
    let mut grown = memory.to_vec();
    grown.resize(words * 32, 0);
    Cow::Owned(grown)
}

/// The state the JSON logger sees when an opcode fails while it runs: the opcode has popped its
/// inputs, and no return data is reported.
fn fault_log(log: &StructLog, op: u8, out_of_gas: bool) -> StructLog {
    let mut fault = log.clone();
    fault.return_data = None;
    let Some(stack) = &mut fault.stack else { return fault };
    // Opcodes that check for a static call first fail before they pop anything.
    let pops = match op {
        op::CREATE | op::CREATE2 if out_of_gas => OpCode::new_or_unknown(op).inputs().into(),
        op::SSTORE | op::TSTORE | op::CREATE | op::CREATE2 | op::SELFDESTRUCT => 0,
        op::LOG0..=op::LOG4 => 0,
        _ => OpCode::new_or_unknown(op).inputs().into(),
    };
    stack.truncate(stack.len().saturating_sub(pops));
    fault
}
