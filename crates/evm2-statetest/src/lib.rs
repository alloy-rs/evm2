//! Ethereum state test execution for evm2.

mod discover;
mod env;
mod error;
mod execute;
mod runner;
mod types;

use std::process::ExitCode;

/// Runs the cargo-nextest state test harness.
pub fn run() -> ExitCode {
    runner::run()
}
