//! EIP-3155 execution tracer.
//!
//! Streams one JSON struct log per executed opcode to a writer (stdout by
//! default), matching the [EIP-3155] `debug_traceTransaction` line format used
//! by `geth --json` and `evmone`. A trailing summary line carries the final
//! output, gas used, and post-state root.
//!
//! Tracing is driven through the [`evm2::Inspector`] `step`/`step_end` hooks, so
//! it only observes execution under the interpreter backend; the JIT/AOT runners
//! bypass instruction-level dispatch.
//!
//! [EIP-3155]: https://eips.ethereum.org/EIPS/eip-3155

use alloy_primitives::{B256, Bytes, U256};
use evm2::{
    EvmTypesHost, Inspector,
    interpreter::{Interpreter, opcode::OpCode},
};
use serde_json::json;
use std::io::{self, Write};

/// Per-opcode state captured in `step`, emitted in `step_end`.
struct PendingStep {
    pc: usize,
    op: u8,
    /// Gas remaining before the opcode executes.
    gas: u64,
    stack: Vec<U256>,
    depth: u16,
    mem_size: usize,
    refund: i64,
}

/// EIP-3155 struct-log tracer.
///
/// Attach with [`evm2::Evm::set_inspector`] before `transact`; call
/// [`write_summary`] after execution to emit the trailing summary line.
pub(crate) struct Eip3155Tracer<W = io::Stdout> {
    out: W,
    pending: Option<PendingStep>,
}

impl Eip3155Tracer<io::Stdout> {
    /// Creates a tracer that writes struct logs to standard output.
    pub(crate) fn to_stdout() -> Self {
        Self::new(io::stdout())
    }
}

impl<W: Write> Eip3155Tracer<W> {
    /// Creates a tracer that writes struct logs to `out`.
    pub(crate) const fn new(out: W) -> Self {
        Self { out, pending: None }
    }

    fn emit(&mut self, step: &PendingStep, gas_cost: u64) {
        let value = json!({
            "pc": step.pc,
            "op": step.op,
            "gas": hex_u64(step.gas),
            "gasCost": hex_u64(gas_cost),
            "stack": step.stack,
            "depth": u64::from(step.depth) + 1,
            "returnData": "0x",
            "refund": hex_u64(step.refund.max(0) as u64),
            "memSize": step.mem_size,
            "opName": OpCode::new_or_unknown(step.op).as_str(),
        });
        // Tracing is best-effort diagnostic output; a broken pipe must not abort
        // execution.
        let _ = writeln!(self.out, "{value}");
    }
}

impl<W: Write, T: EvmTypesHost> Inspector<T> for Eip3155Tracer<W> {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.pending = Some(PendingStep {
            pc: interp.pc(),
            op: interp.opcode(),
            gas: interp.gas().remaining(),
            stack: interp.stack().as_slice().to_vec(),
            depth: interp.message().depth,
            mem_size: interp.memory().len(),
            refund: interp.gas().refunded(),
        });
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let Some(step) = self.pending.take() else {
            return;
        };
        let gas_cost = step.gas.saturating_sub(interp.gas().remaining());
        self.emit(&step, gas_cost);
    }
}

/// Writes the EIP-3155 summary line (final output, gas used, post-state root).
pub(crate) fn write_summary<W: Write>(
    out: &mut W,
    output: &Bytes,
    gas_used: u64,
    state_root: B256,
) {
    let value = json!({
        "stateRoot": state_root,
        "output": output,
        "gasUsed": hex_u64(gas_used),
    });
    let _ = writeln!(out, "{value}");
}

/// Formats a `u64` as a `0x`-prefixed minimal hex quantity.
fn hex_u64(value: u64) -> String {
    format!("{value:#x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_line_is_valid_json_with_hex_gas() {
        let mut buf = Vec::new();
        write_summary(&mut buf, &Bytes::from_static(&[0x01, 0x02]), 21_000, B256::ZERO);
        let line = String::from_utf8(buf).unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["gasUsed"], "0x5208");
        assert_eq!(value["output"], "0x0102");
        assert_eq!(
            value["stateRoot"],
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn hex_u64_is_minimal_quantity() {
        assert_eq!(hex_u64(0), "0x0");
        assert_eq!(hex_u64(255), "0xff");
    }
}
