//! Ethereum Execution Spec Tests for evm2.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod binary;
pub mod blockchaintest;
mod custom;
mod discover;
mod env;
mod error;
mod execute;
mod filter;
mod fixture_io;
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
    Hook as BlockchainTestHook, NoopHook as BlockchainTestNoopHook,
    TestError as BlockchainTestError, TransactionFailed as BlockchainTestTransactionFailed,
    TransactionFinished as BlockchainTestTransactionFinished,
    TransactionStarted as BlockchainTestTransactionStarted,
    execute_str as execute_blockchain_tests_str, execute_suite as execute_blockchain_tests_suite,
    run as run_blockchaintests,
};
pub use error::TestError as StateTestError;
pub use execute::{
    ExecuteConfig as StateTestExecuteConfig, ExecuteSummary as StateTestExecuteSummary,
    execute_str_with_config as execute_state_tests_str,
    execute_str_with_filter as execute_state_tests_str_with_filter,
};
pub use filter::EntryPoint;
pub use fixture_io::{
    FixtureReadError, FixtureWriteError, is_binary_path as is_binary_fixture_path,
    read_blockchain as read_blockchain_fixture, read_to_string as read_fixture_text,
    write_blockchain as write_blockchain_fixture,
};
pub use runner::run as run_statetests;
pub use tx::{AccessListItem, TestAuthorization};
pub use types::{
    AccountInfo as StateTestAccountInfo, Env as StateTestEnv, SpecName as StateTestSpecName,
    Test as StateTestPost, TestSuite as StateTestSuite, TestUnit as StateTestUnit,
    TransactionParts as StateTestTransactionParts, TxPartIndices as StateTestTxPartIndices,
};

/// Runs all EEST harnesses.
///
/// When `EVM2_FIXTURE_PATH` is set, only that folder (or file) runs, as a single
/// suite that detects each fixture's kind, so no test-name filter is required.
pub fn run() -> std::process::ExitCode {
    if let Some(path) = fixtures::custom_fixture_path() {
        return harness::run_json_harnesses(vec![custom::suite(path)]);
    }
    harness::run_json_harnesses(vec![runner::suite(), blockchaintest::suite()])
}
