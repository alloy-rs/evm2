#![allow(missing_docs, clippy::missing_const_for_fn)]

use criterion::{Criterion, criterion_group, criterion_main};
use evm2_cli::evm_bench::{self, BenchCase, BenchCaseKind, BenchKind};
use std::{env, time::Duration};

#[path = "evm/fixture.rs"]
mod fixture;
#[path = "evm/mainnet.rs"]
mod mainnet;
#[path = "evm/revm.rs"]
mod revm;
#[path = "evm/support.rs"]
mod support;

fn evm(c: &mut Criterion) {
    let mut group = c.benchmark_group("evm");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(10);

    let benches = evm_bench::BENCHES;
    let suites =
        fixture::Suites::load(benches.iter().filter_map(|bench| bench.transaction_fixture_path()));
    let cases = expand_cases(benches, &suites);
    let bench_revm = env::var_os("EVM2_BENCH_REVM").is_some();
    for bench in &cases {
        match bench.kind {
            BenchCaseKind::Transaction { .. } => {
                let prepared = support::PreparedBench::load(bench, &suites);
                prepared.sanity_check();
                prepared.bench(&mut group);

                if bench_revm {
                    let prepared = revm::PreparedBench::load(bench, &suites);
                    prepared.sanity_check();
                    prepared.bench(&mut group);
                }
            }
            BenchCaseKind::BlockchainReplay => {
                let prepared = mainnet::PreparedBench::load(bench);
                prepared.sanity_check();
                prepared.bench(&mut group);
            }
        }
    }

    group.finish();
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
