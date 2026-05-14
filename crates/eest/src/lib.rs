//! Ethereum Execution Spec Tests for evm2.

#![warn(unused_crate_dependencies)]

mod blockchaintest;
mod discover;
mod env;
mod error;
mod execute;
mod fixtures;
mod harness;
mod runner;
mod state;
mod tx;
mod types;

pub use blockchaintest::run as run_blockchaintests;
pub use runner::run as run_statetests;

/// Runs all EEST harnesses.
pub fn run() -> std::process::ExitCode {
    harness::run_json_harnesses(vec![runner::suite(), blockchaintest::suite()])
}
