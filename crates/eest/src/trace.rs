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
    interpreter::{Interpreter, Message, MessageResult, opcode::OpCode},
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

    /// Emits the pending step, if any, with a gas cost derived from `interp`'s
    /// remaining gas.
    ///
    /// On out-of-gas the interpreter zeroes the remaining gas before calling
    /// `step_end`, so subtracting it would report every gas unit left at the
    /// start of the step instead of the opcode's cost. `failed_charge` carries
    /// the charge the opcode actually attempted; geth reports that value too.
    ///
    /// Frame-spawning opcodes (CALL/CREATE families) never reach `step_end`:
    /// dispatch unwinds into the frame machinery instead, so the pending step
    /// is flushed from the `call`/`create` hooks, which fire on the spawning
    /// frame before the child executes. The gas cost then covers everything
    /// charged so far, including the gas forwarded to the child.
    fn flush_pending<T: EvmTypesHost>(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let Some(step) = self.pending.take() else {
            return;
        };
        let gas_cost = interp
            .failed_charge()
            .unwrap_or_else(|| step.gas.saturating_sub(interp.gas().remaining()));
        self.emit(&step, gas_cost);
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
        self.flush_pending(interp);
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        _message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.flush_pending(interp);
        None
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        _message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.flush_pending(interp);
        None
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

    use alloy_primitives::{Address, Bytes};
    use evm2::{
        BaseEvmTypes, Evm, Precompiles, SpecId,
        bytecode::Bytecode,
        env::{BlockEnvExt, TxEnvExt},
        evm::InMemoryDB,
        interpreter::{Host, MessageExt, op},
        registry::TxRegistry,
    };

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

    #[test]
    fn struct_log_reports_attempted_charge_on_out_of_gas() {
        // A cold zero-to-nonzero SSTORE costs 22100; the message leaves exactly 5000 for it
        // after the two pushes, so the step runs out of gas.
        let contract = Address::with_last_byte(0xc0);
        let mut out = Vec::new();
        // The tracer borrows `out`, so the EVM has to be dropped before it is read again.
        let result = {
            let mut evm = Evm::<BaseEvmTypes>::new(
                SpecId::LONDON,
                BlockEnvExt::default(),
                TxRegistry::new(),
                InMemoryDB::default(),
                Precompiles::base(SpecId::LONDON),
            );
            evm.set_inspector(Eip3155Tracer::new(&mut out));

            let mut message = MessageExt {
                destination: contract,
                code_address: contract,
                gas_limit: 5_000 + 6,
                code: Bytecode::new_legacy(Bytes::from_static(&[
                    op::PUSH1,
                    0x01,
                    op::PUSH1,
                    0x00,
                    op::SSTORE,
                ])),
                ..Default::default()
            };
            Host::execute_message(&mut evm, &TxEnvExt::default(), &mut message).unwrap()
        };
        assert!(!result.stop.is_success(), "{result:#?}");

        let logs = String::from_utf8(out).unwrap();
        let last = logs.lines().last().expect("no struct logs were emitted");
        let value: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(value["opName"], "SSTORE");
        // Gas left before the opcode, not the charge it failed to pay.
        assert_eq!(value["gas"], "0x1388");
        // geth reports the attempted charge: 2100 cold slot load + 20000 SSTORE set.
        assert_eq!(value["gasCost"], "0x5654");
    }
}
