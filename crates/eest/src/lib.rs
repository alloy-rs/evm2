//! Ethereum Execution Spec Tests for evm2.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod blockchaintest;
mod discover;
mod env;
mod error;
mod execute;
mod filter;
mod fixtures;
mod harness;
mod runner;
mod state;
mod tx;
mod types;

pub use blockchaintest::{
    ExecuteConfig as BlockchainTestExecuteConfig, ExecuteSummary as BlockchainTestExecuteSummary,
    TestError as BlockchainTestError, execute_str_with_config as execute_blockchain_tests_str,
    execute_str_with_filter as execute_blockchain_tests_str_with_filter,
    run as run_blockchaintests,
};
pub use error::TestError as StateTestError;
pub use execute::{
    ExecuteConfig as StateTestExecuteConfig, ExecuteSummary as StateTestExecuteSummary,
    execute_str_with_config as execute_state_tests_str,
    execute_str_with_filter as execute_state_tests_str_with_filter,
};
pub use filter::EntryPoint;
pub use runner::run as run_statetests;
pub use tx::{AccessListItem, TestAuthorization};

/// Runs all EEST harnesses.
pub fn run() -> std::process::ExitCode {
    harness::run_json_harnesses(vec![runner::suite(), blockchaintest::suite()])
}
