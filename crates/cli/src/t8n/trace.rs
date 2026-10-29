//! Tracer selection and trace rendering on top of `evm2-inspectors`.

use super::{Args, Error, output::TxTrace};
use alloy_primitives::{B256, hex};
use alloy_rpc_types_trace::geth::{
    DefaultFrame, GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace, StructLog,
};
use evm2::{
    BaseEvmTypes, TxResultWithState,
    env::BlockEnv,
    ethereum::RecoveredTxEnvelope,
    evm::DynDatabase,
    interpreter::{InstrStop, opcode::OpCode},
};
use evm2_inspectors::tracing::DebugInspector;
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
}

impl Tracing {
    /// No tracing.
    pub fn none() -> Self {
        Self::default()
    }

    /// EIP-3155 lines, or the default struct logger frame when `structlog` is set.
    pub fn struct_logger(kind: TraceKind, memory: bool, no_stack: bool, return_data: bool) -> Self {
        let config = GethDefaultTracingOptions {
            enable_memory: Some(memory),
            disable_stack: Some(no_stack),
            disable_storage: Some(kind == TraceKind::Jsonl),
            enable_return_data: Some(return_data),
            ..Default::default()
        };
        Self { kind: Some(kind), options: GethDebugTracingOptions { config, ..Default::default() } }
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
        Ok(Self { kind: Some(TraceKind::Tracer), options })
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
        let trace = inspector
            .get_result(None, tx, block, result, db)
            .map_err(|err| Error::Tracer(err.to_string()))?;
        let (extension, content) = match self.kind {
            Some(TraceKind::Jsonl) => {
                let GethTrace::Default(frame) = trace else {
                    return Err(Error::Tracer("struct logger returned another frame".into()));
                };
                ("jsonl", jsonl(inspector, &frame, result))
            }
            _ => ("json", serde_json::to_string(&trace).expect("trace serializes")),
        };
        Ok(TxTrace { index, hash, extension, content })
    }
}

/// Renders a struct logger frame as EIP-3155 lines followed by the summary line.
fn jsonl(
    inspector: &DebugInspector,
    frame: &DefaultFrame,
    result: &TxResultWithState<BaseEvmTypes>,
) -> String {
    let opcodes = (0..=u8::MAX).map(|byte| (OpCode::new_or_unknown(byte).as_str(), byte)).collect();
    let mut out = String::new();
    for log in &frame.struct_logs {
        out.push_str(&step_line(log, &opcodes).to_string());
        out.push('\n');
    }
    let (output, gas_used) = match inspector {
        DebugInspector::Default(tracing, _) => {
            tracing.traces().nodes().first().map_or((Default::default(), 0), |node| {
                (node.trace.output.clone(), node.trace.gas_used)
            })
        }
        _ => Default::default(),
    };
    let mut summary =
        json!({ "output": hex::encode(&output), "gasUsed": format!("{gas_used:#x}") });
    let error = frame
        .struct_logs
        .iter()
        .rev()
        .find(|log| log.depth == 1 && log.error.is_some())
        .and_then(|log| log.error.clone())
        .or_else(|| (result.result.stop == InstrStop::Revert).then(|| "execution reverted".into()));
    if frame.failed
        && let Some(error) = error
    {
        summary["error"] = json!(error);
    }
    out.push_str(&summary.to_string());
    out.push('\n');
    out
}

fn step_line(log: &StructLog, opcodes: &HashMap<&str, u8>) -> Value {
    let mut line = json!({
        "pc": log.pc,
        "op": opcodes.get(log.op.as_ref()).copied().unwrap_or(0xfe),
        "gas": format!("{:#x}", log.gas),
        "gasCost": format!("{:#x}", log.gas_cost),
    });
    line["memSize"] = json!(log.memory.as_ref().map_or(0, |memory| memory.len() * 32));
    if let Some(memory) = &log.memory
        && !memory.is_empty()
    {
        let words = memory.iter().map(|word| word.trim_start_matches("0x")).collect::<String>();
        line["memory"] = json!(format!("0x{words}"));
    }
    line["stack"] = json!(log.stack.as_deref().unwrap_or_default());
    if let Some(return_data) = &log.return_data
        && !return_data.is_empty()
    {
        line["returnData"] = json!(return_data);
    }
    line["depth"] = json!(log.depth);
    line["refund"] = json!(log.refund_counter.unwrap_or_default());
    line["opName"] = json!(log.op);
    if let Some(error) = &log.error {
        line["error"] = json!(error);
    }
    line
}
