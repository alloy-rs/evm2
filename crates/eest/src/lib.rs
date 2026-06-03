//! Ethereum Execution Spec Tests for evm2.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::{ffi::OsString, process::ExitCode};

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
pub fn run() -> ExitCode {
    harness::run_json_harnesses(vec![runner::suite(), blockchaintest::suite()])
}

/// Runs the state test harness directly.
pub fn run_statetests_direct() -> ExitCode {
    harness::run_json_harnesses_direct(vec![runner::suite()])
}

/// Runs the blockchain test harness directly.
pub fn run_blockchaintests_direct() -> ExitCode {
    harness::run_json_harnesses_direct(vec![blockchaintest::suite()])
}

/// Runs all EEST harnesses directly.
pub fn run_direct() -> ExitCode {
    harness::run_json_harnesses_direct(vec![runner::suite(), blockchaintest::suite()])
}

/// Runs the state test harness directly with explicit command line arguments.
pub fn run_statetests_direct_from_iter<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    harness::run_json_harnesses_direct_from_iter(vec![runner::suite()], args)
}

/// Runs the blockchain test harness directly with explicit command line arguments.
pub fn run_blockchaintests_direct_from_iter<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    harness::run_json_harnesses_direct_from_iter(vec![blockchaintest::suite()], args)
}

/// Runs all EEST harnesses directly with explicit command line arguments.
pub fn run_direct_from_iter<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    harness::run_json_harnesses_direct_from_iter(
        vec![runner::suite(), blockchaintest::suite()],
        args,
    )
}
