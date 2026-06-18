#![allow(missing_docs, clippy::missing_const_for_fn)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::{env, time::Duration};

#[path = "evm/cases.rs"]
mod cases;
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

    let cases = cases::all();
    let suites =
        fixture::Suites::load(cases.iter().filter_map(|bench| bench.transaction_fixture_path()));
    let bench_revm = env::var_os("EVM2_BENCH_REVM").is_some();
    for bench in cases {
        match bench.kind {
            cases::BenchKind::Transaction { .. } => {
                let prepared = support::PreparedBench::load(bench, &suites);
                prepared.sanity_check();
                prepared.bench(&mut group);

                if bench_revm {
                    let prepared = revm::PreparedBench::load(bench, &suites);
                    prepared.sanity_check();
                    prepared.bench(&mut group);
                }
            }
            cases::BenchKind::BlockchainReplay => {
                let prepared = mainnet::PreparedBench::load(bench);
                prepared.sanity_check();
                prepared.bench(&mut group);
            }
        }
    }

    group.finish();
}

criterion_group!(benches, evm);
criterion_main!(benches);
