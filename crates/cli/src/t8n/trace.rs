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
use std::collections::HashMap;

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
            // A halt of the top frame leaves no step error; its frame error says what failed.
            let error = log.error.clone().or_else(|| {
                (position == last
                    && !matches!(op, op::STOP | op::RETURN | op::REVERT | op::SELFDESTRUCT))
                .then(|| frame_error.clone())
                .flatten()
            });
            if let Some(error) = error {
                line["error"] = json!(error);
            }
            self.line(line);
            if op == op::SELFDESTRUCT && node.trace.selfdestruct_address().is_some() {
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
    if let Some(cost) = log.state_gas_cost.filter(|cost| *cost != 0) {
        line["stateGasCost"] = json!(cost);
    }
    if let Some(reservoir) = log.state_gas_reservoir.filter(|reservoir| *reservoir != 0) {
        line["stateGasReservoir"] = json!(format!("{reservoir:#x}"));
    }
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
