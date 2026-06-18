use crate::{
    args::Replay,
    error::{Error, Result},
    fixture::{self, FixtureKind},
};
use alloy_primitives::U256;
use evm2_eest::{
    BlockchainTestBlockFailed, BlockchainTestBlockFinished, BlockchainTestBlockStarted,
    BlockchainTestCaseStarted, BlockchainTestExecuteConfig, BlockchainTestHook,
    BlockchainTestTransactionFailed, BlockchainTestTransactionFinished,
    BlockchainTestTransactionStarted, EntryPoint, StateTestExecuteConfig,
    execute_blockchain_tests_str, execute_state_tests_str_with_filter,
};
use std::time::Instant;

pub(crate) fn run(command: Replay) -> Result<()> {
    let input = fixture::read_text(&command.path)?;
    let entrypoint = EntryPoint::new(command.entrypoint);
    match fixture::detect_str(&command.path, &input)? {
        Some(FixtureKind::StateTest) => {
            let summary = execute_state_tests_str_with_filter(
                &command.path,
                &input,
                StateTestExecuteConfig { db_stats: command.db_stats, ..Default::default() },
                &entrypoint,
            )
            .map_err(|source| Error::StateTest { source })?;
            println!(
                "replayed state fixture {}: {} executed, {} skipped",
                command.path.display(),
                summary.executed,
                summary.skipped
            );
            Ok(())
        }
        Some(FixtureKind::BlockchainTest) => {
            let mut hook = ReplayProgressHook::default();
            let summary = execute_blockchain_tests_str(
                &command.path,
                &input,
                BlockchainTestExecuteConfig { db_stats: command.db_stats, ..Default::default() },
                &entrypoint,
                &mut hook,
            )
            .map_err(|source| Error::BlockchainTest { source })?;
            println!(
                "replayed blockchain fixture {}: {} executed, {} skipped",
                command.path.display(),
                summary.executed,
                summary.skipped
            );
            Ok(())
        }
        None => Err(Error::UnknownFixtureKind { path: command.path }),
    }
}

#[derive(Default)]
struct ReplayProgressHook {
    block_started_at: Option<Instant>,
    printed_transaction_failure: bool,
    case_name: Option<String>,
    case_elapsed_sec: f64,
    case_gas_used: u128,
    case_blocks_with_gas: usize,
}

impl BlockchainTestHook for ReplayProgressHook {
    fn case_started(&mut self, event: BlockchainTestCaseStarted<'_>) {
        self.case_name = Some(event.name.to_owned());
        self.case_elapsed_sec = 0.0;
        self.case_gas_used = 0;
        self.case_blocks_with_gas = 0;
        eprintln!(
            "replay case {}: {} blocks, network {:?}",
            event.name, event.total_blocks, event.network
        );
    }

    fn block_started(&mut self, event: BlockchainTestBlockStarted) {
        self.block_started_at = Some(Instant::now());
        self.printed_transaction_failure = false;
        eprintln!(
            "replay block {}/{} number={} started ({} txs)",
            event.block_index + 1,
            event.total_blocks,
            display_block_number(event.block_number, event.block_index),
            event.total_transactions
        );
    }

    fn block_finished(&mut self, event: BlockchainTestBlockFinished) {
        let elapsed = self.take_block_elapsed();
        self.record_block(event.block_gas_used, elapsed);
        if let Some(ggas_per_second) = ggas_per_second(event.block_gas_used, elapsed) {
            eprintln!(
                "replay block {}/{} number={} done in {:.2}s ({:.3} Ggas/s)",
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index),
                elapsed,
                ggas_per_second
            );
        } else {
            eprintln!(
                "replay block {}/{} number={} done in {:.2}s",
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index),
                elapsed
            );
        }

        if event.block_index + 1 == event.total_blocks {
            self.print_case_summary(event.total_blocks);
        }
    }

    fn block_failed(&mut self, event: BlockchainTestBlockFailed<'_>) {
        let elapsed = self.take_block_elapsed();
        if self.printed_transaction_failure {
            eprintln!(
                "replay block {}/{} number={} failed in {:.2}s after transaction failure",
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index),
                elapsed
            );
        } else {
            eprintln!(
                "replay block {}/{} number={} failed in {:.2}s: {}",
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index),
                elapsed,
                event.error
            );
        }
    }

    fn transaction_started(&mut self, event: BlockchainTestTransactionStarted) {
        if event.total_transactions >= 100
            && (event.transaction_index == 0 || (event.transaction_index + 1).is_multiple_of(100))
        {
            eprintln!(
                "replay tx {}/{} in block {}/{} number={} started",
                event.transaction_index + 1,
                event.total_transactions,
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index)
            );
        }
    }

    fn transaction_finished(&mut self, event: BlockchainTestTransactionFinished) {
        if event.total_transactions >= 100
            && ((event.transaction_index + 1).is_multiple_of(100)
                || event.transaction_index + 1 == event.total_transactions)
        {
            eprintln!(
                "replay tx {}/{} in block {}/{} number={} done",
                event.transaction_index + 1,
                event.total_transactions,
                event.block_index + 1,
                event.total_blocks,
                display_block_number(event.block_number, event.block_index)
            );
        }
    }

    fn transaction_failed(&mut self, event: BlockchainTestTransactionFailed<'_>) {
        self.printed_transaction_failure = true;
        eprintln!(
            "replay tx {}/{} in block {}/{} number={} failed: {}",
            event.transaction_index + 1,
            event.total_transactions,
            event.block_index + 1,
            event.total_blocks,
            display_block_number(event.block_number, event.block_index),
            event.error
        );
    }
}

impl ReplayProgressHook {
    fn take_block_elapsed(&mut self) -> f64 {
        self.block_started_at.take().map(|started| started.elapsed().as_secs_f64()).unwrap_or(0.0)
    }

    fn record_block(&mut self, block_gas_used: Option<U256>, elapsed: f64) {
        self.case_elapsed_sec += elapsed;
        if let Some(gas_used) = block_gas_used.map(|gas| gas.saturating_to::<u128>()) {
            self.case_gas_used = self.case_gas_used.saturating_add(gas_used);
            self.case_blocks_with_gas += 1;
        }
    }

    fn print_case_summary(&self, total_blocks: usize) {
        let Some(case_name) = &self.case_name else {
            return;
        };
        let Some(ggas_per_second) =
            ggas_per_second_from_gas(self.case_gas_used, self.case_elapsed_sec)
        else {
            eprintln!(
                "replay case {}: {} blocks done in {:.2}s",
                case_name, total_blocks, self.case_elapsed_sec
            );
            return;
        };

        if self.case_blocks_with_gas == total_blocks {
            eprintln!(
                "replay case {}: {} blocks done in {:.2}s ({:.3} Ggas/s aggregate, {:.3} Ggas total)",
                case_name,
                total_blocks,
                self.case_elapsed_sec,
                ggas_per_second,
                ggas(self.case_gas_used)
            );
        } else {
            eprintln!(
                "replay case {}: {} blocks done in {:.2}s ({:.3} Ggas/s aggregate, {:.3} Ggas total across {}/{} blocks with gasUsed)",
                case_name,
                total_blocks,
                self.case_elapsed_sec,
                ggas_per_second,
                ggas(self.case_gas_used),
                self.case_blocks_with_gas,
                total_blocks
            );
        }
    }
}

fn display_block_number(block_number: Option<U256>, fallback: usize) -> String {
    block_number.map(|number| number.to_string()).unwrap_or_else(|| fallback.to_string())
}

fn ggas_per_second(block_gas_used: Option<U256>, elapsed: f64) -> Option<f64> {
    ggas_per_second_from_gas(block_gas_used?.saturating_to::<u128>(), elapsed)
}

fn ggas_per_second_from_gas(gas_used: u128, elapsed: f64) -> Option<f64> {
    (elapsed > 0.0).then_some(ggas(gas_used) / elapsed)
}

fn ggas(gas_used: u128) -> f64 {
    gas_used as f64 / 1_000_000_000.0
}
