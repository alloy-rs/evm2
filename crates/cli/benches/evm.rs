#![allow(missing_docs, unexpected_cfgs, clippy::missing_const_for_fn)]

use criterion::{
    BenchmarkGroup, Criterion, criterion_group, criterion_main, measurement::WallTime,
};
use evm2_cli::evm_bench::{self, BenchCase, BenchCaseKind, BenchKind};
use std::{env, time::Duration};

#[path = "evm/fixture.rs"]
mod fixture;
#[cfg(feature = "jit")]
#[path = "evm/jit.rs"]
mod jit;
#[path = "evm/mainnet.rs"]
mod mainnet;
#[path = "evm/mainnet_revm.rs"]
mod mainnet_revm;
#[path = "evm/revm.rs"]
mod revm;
#[path = "evm/support.rs"]
mod support;

const WARM_UP: Duration = Duration::from_secs(1);
const MEASUREMENT: Duration = Duration::from_secs(2);
const SAMPLE_SIZE: usize = 10;

/// Blockchain-replay cases run ~110 ms per iteration, so the default budget
/// collects 10 samples from 20 iterations -- too thin to compare two engines.
/// Replay cases therefore get their own budget, overridable through
/// `EVM2_BENCH_REPLAY_MEASUREMENT_SECS` and `EVM2_BENCH_REPLAY_SAMPLES`.
const REPLAY_WARM_UP: Duration = Duration::from_secs(3);
const REPLAY_MEASUREMENT_SECS: u64 = 15;
const REPLAY_SAMPLE_SIZE: usize = 20;

fn evm(c: &mut Criterion) {
    let mut group = c.benchmark_group("evm");
    apply_default_budget(&mut group);

    let benches = evm_bench::BENCHES;
    let suites =
        fixture::Suites::load(benches.iter().filter_map(|bench| bench.transaction_fixture_path()));
    let cases = expand_cases(benches, &suites);

    let bench_revm = env::var_os("EVM2_BENCH_REVM").is_some();

    #[cfg(feature = "jit")]
    let mut jit_compiler = jit::Compiler::new();

    for bench in &cases {
        match bench.kind {
            BenchCaseKind::Transaction { .. } => {
                let prepared = support::PreparedBench::load(bench, &suites);
                prepared.sanity_check();
                prepared.bench(&mut group);

                #[cfg(feature = "jit")]
                if let Some(prepared) = jit::PreparedBench::load(bench, &suites, &mut jit_compiler)
                {
                    prepared.sanity_check();
                    prepared.bench(&mut group);
                }

                if bench_revm {
                    let prepared = revm::PreparedBench::load(bench, &suites);
                    prepared.sanity_check();
                    prepared.bench(&mut group);
                }
            }
            BenchCaseKind::BlockchainReplay => {
                let prepared = mainnet::PreparedBench::load(bench);
                prepared.sanity_check();

                let revm_prepared = bench_revm.then(|| {
                    let prepared = mainnet_revm::PreparedBench::load(bench);
                    prepared.sanity_check();
                    prepared
                });

                apply_replay_budget(&mut group);
                prepared.bench(&mut group);
                if let Some(revm_prepared) = &revm_prepared {
                    revm_prepared.bench(&mut group);
                    revm_prepared.bench_setup(&mut group);
                }
                apply_default_budget(&mut group);
            }
        }
    }

    group.finish();
}

fn apply_default_budget(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.warm_up_time(WARM_UP);
    group.measurement_time(MEASUREMENT);
    group.sample_size(SAMPLE_SIZE);
}

fn apply_replay_budget(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.warm_up_time(REPLAY_WARM_UP);
    group.measurement_time(Duration::from_secs(env_or(
        "EVM2_BENCH_REPLAY_MEASUREMENT_SECS",
        REPLAY_MEASUREMENT_SECS,
    )));
    group.sample_size(env_or("EVM2_BENCH_REPLAY_SAMPLES", REPLAY_SAMPLE_SIZE));
}

fn env_or<T: std::str::FromStr>(name: &str, fallback: T) -> T {
    env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(fallback)
}

fn expand_cases(benches: &[evm_bench::Bench], suites: &fixture::Suites) -> Vec<BenchCase> {
    let mut cases = Vec::new();
    for bench in benches {
        match bench.kind {
            BenchKind::Transaction { spec } => {
                cases.push(BenchCase::transaction(bench.name, spec, bench.fixture_path));
            }
            BenchKind::TransactionSuite { spec } => {
                let suite = suites.get(bench.fixture_path);
                cases.extend(
                    suite.case_names().map(|name| {
                        BenchCase::transaction(name.to_owned(), spec, bench.fixture_path)
                    }),
                );
            }
            BenchKind::BlockchainReplay => {
                cases.push(BenchCase::blockchain_replay(bench.name, bench.fixture_path));
            }
        }
    }
    cases
}

criterion_group!(benches, evm);
criterion_main!(benches);
