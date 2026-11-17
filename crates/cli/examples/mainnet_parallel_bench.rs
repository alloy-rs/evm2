//! Times mainnet block replay with sequential and parallel transaction execution.
//!
//! ```sh
//! cargo run --release -p evm2-cli --example mainnet_parallel_bench -- \
//!     data/mainnet-25347446-25347455.bin.zst --threads 1,2,4,8 --runs 15
//! ```
//!
//! Each run replays every block of the fixture's first case from a fresh copy of its pre-state,
//! with receipt roots checked against the fixture headers, and prints one CSV row per thread count
//! with the median, minimum and median absolute deviation of the runs. A thread count of 0 is
//! sequential execution with `BlockExecutor::execute_transaction`.

use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestHook, BlockchainTestNoopHook,
    BlockchainTestTransactionFinished, blockchain_test_pre_state, execute_blockchain_test_case_on,
    read_blockchain_fixture,
};
use std::{num::NonZeroUsize, path::PathBuf, time::Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut threads = vec![0, 1, 2, 4, 8];
    let mut runs = 15;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--threads" => {
                threads = args
                    .next()
                    .expect("--threads needs a list")
                    .split(',')
                    .map(|n| n.parse().expect("thread count"))
                    .collect();
            }
            "--runs" => runs = args.next().expect("--runs needs a count").parse().expect("runs"),
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let path = path.expect("fixture path");
    let suite = read_blockchain_fixture(&path).expect("read fixture");
    let (case_name, case) = suite.0.first_key_value().expect("fixture has a case");
    let pre_state = blockchain_test_pre_state(&path, case_name, case).expect("pre-state");
    let mut counter = CountTransactions(0);
    execute_blockchain_test_case_on(
        &path,
        case_name,
        case,
        &mut pre_state.clone(),
        BlockchainTestExecuteConfig { validate: false, ..Default::default() },
        &mut counter,
    )
    .expect("sequential replay");
    let transactions = counter.0;
    println!("fixture,blocks,transactions,threads,runs,median_ms,min_ms,mad_ms,speedup");
    let mut sequential_median = None;
    for &count in &threads {
        let config = BlockchainTestExecuteConfig {
            validate: false,
            compare_receipt_root: true,
            parallel_threads: NonZeroUsize::new(count),
            ..Default::default()
        };
        let mut times = Vec::with_capacity(runs);
        // One warm-up run, then the timed ones.
        for run in 0..=runs {
            let mut database = pre_state.clone();
            let start = Instant::now();
            execute_blockchain_test_case_on(
                &path,
                case_name,
                case,
                &mut database,
                config,
                &mut BlockchainTestNoopHook,
            )
            .unwrap_or_else(|err| panic!("replay with {count} threads failed: {err}"));
            let elapsed = start.elapsed().as_secs_f64() * 1e3;
            drop(database);
            if run > 0 {
                times.push(elapsed);
            }
        }
        times.sort_by(f64::total_cmp);
        let median = times[times.len() / 2];
        let mut deviations: Vec<_> = times.iter().map(|t| (t - median).abs()).collect();
        deviations.sort_by(f64::total_cmp);
        let mad = deviations[deviations.len() / 2];
        let baseline = *sequential_median.get_or_insert(median);
        println!(
            "{},{},{transactions},{count},{runs},{median:.3},{:.3},{mad:.3},{:.3}",
            path.display(),
            case.blocks.len(),
            times[0],
            baseline / median,
        );
    }
}

struct CountTransactions(usize);

impl BlockchainTestHook for CountTransactions {
    fn transaction_finished(&mut self, _: BlockchainTestTransactionFinished<'_>) {
        self.0 += 1;
    }
}
