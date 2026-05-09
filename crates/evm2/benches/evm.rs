#![allow(missing_docs, clippy::missing_const_for_fn)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::time::Duration;

#[path = "evm/cases.rs"]
mod cases;
#[path = "evm/fixture.rs"]
mod fixture;
#[path = "evm/revm.rs"]
mod revm;
#[path = "evm/support.rs"]
mod support;

fn evm(c: &mut Criterion) {
    let suites = fixture::Suites::load(cases::all().iter().map(|bench| bench.fixture_path));
    for bench in cases::all() {
        let mut group = c.benchmark_group("evm");
        group.warm_up_time(Duration::from_secs(1));
        group.sample_size(sample_size(bench.name));

        let prepared = support::PreparedBench::load(bench, &suites);
        prepared.sanity_check();
        prepared.bench(&mut group);

        let prepared = revm::PreparedBench::load(bench, &suites);
        prepared.sanity_check();
        prepared.bench(&mut group);

        group.finish();
    }
}

fn sample_size(name: &str) -> usize {
    match name {
        "onchain_lm_v2" => 10,
        "snailtracer" | "burntpix" => 20,
        "erc20_transfer" | "hash_10k" => 30,
        _ => 100,
    }
}

criterion_group!(benches, evm);
criterion_main!(benches);
