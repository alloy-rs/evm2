//! Gas costs of out-of-gas steps as geth reports them in structured logs.
//!
//! The consumed gas of such a step is the whole frame remainder, but geth reports the cost it
//! computed for the opcode before halting instead.

use evm2::{EvmTypes, interpreter::Interpreter};

/// Returns the gas cost geth reports for the current step if it ran out of gas, and `None`
/// otherwise.
///
/// Called from `step_end`, where the remaining gas has already been zeroed. The default is the
/// failed charge; add opcodes where geth's cost differs from it here.
pub(super) const fn oog_gas_cost<T: EvmTypes>(interp: &Interpreter<'_, '_, T>) -> Option<u64> {
    interp.failed_charge()
}
