//! The gas cost a tracer reports for a step.
use evm2::interpreter::{InstrStop, Result};

/// Returns the gas cost of a step that stopped with `result`.
///
/// A step that ran out of gas costs the whole `gas_remaining` it started with, except the
/// reentrancy sentry, which rejects SSTORE before charging any gas and costs 0. Any other step
/// costs what it `charged`.
pub(crate) fn step_cost(result: Result, gas_remaining: u64, charged: u64) -> u64 {
    match result {
        Err(InstrStop::ReentrancySentryOOG) => 0,
        Err(stop) if stop.is_out_of_gas() => gas_remaining,
        _ => charged,
    }
}
