//! Ethereum Execution Spec Tests for evm2.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod blockchaintest;
mod discover;
mod env;
mod error;
mod execute;
mod filter;
mod fixtures;
mod forks;
mod harness;
mod runner;
mod state;
mod tx;
mod types;

pub use blockchaintest::{
    BlockFailed as BlockchainTestBlockFailed, BlockFinished as BlockchainTestBlockFinished,
    BlockStarted as BlockchainTestBlockStarted, CaseStarted as BlockchainTestCaseStarted,
    ExecuteConfig as BlockchainTestExecuteConfig, ExecuteSummary as BlockchainTestExecuteSummary,
    ExecutionMode as BlockchainTestExecutionMode, Hook as BlockchainTestHook,
    NoopHook as BlockchainTestNoopHook, TestError as BlockchainTestError,
    TransactionFailed as BlockchainTestTransactionFailed,
    TransactionFinished as BlockchainTestTransactionFinished,
    TransactionStarted as BlockchainTestTransactionStarted,
    execute_str as execute_blockchain_tests_str, run as run_blockchaintests,
};
pub use error::TestError as StateTestError;
pub use execute::{
    ExecuteConfig as StateTestExecuteConfig, ExecuteSummary as StateTestExecuteSummary,
    ExecutionMode as StateTestExecutionMode, execute_str_with_config as execute_state_tests_str,
    execute_str_with_filter as execute_state_tests_str_with_filter,
};
pub use filter::EntryPoint;
pub use runner::run as run_statetests;
pub use tx::{AccessListItem, TestAuthorization};

/// Runs all EEST harnesses.
pub fn run() -> std::process::ExitCode {
    let mut suites = runner::suites();
    suites.extend(blockchaintest::suites());
    harness::run_json_harnesses(suites)
}
