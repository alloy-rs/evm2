#![allow(missing_docs, clippy::missing_const_for_fn)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::time::Duration;

#[path = "evm/cases.rs"]
mod cases;
#[path = "evm/fixture.rs"]
mod fixture;
#[path = "evm/mainnet.rs"]
mod mainnet;
#[path = "evm/support.rs"]
mod support;

fn evm(c: &mut Criterion) {
    let mut group = c.benchmark_group("evm");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(10);

    let suites = fixture::Suites::load(cases::all().iter().map(|bench| bench.fixture_path));
    for bench in cases::all() {
        let prepared = support::PreparedBench::load(bench, &suites);
        prepared.sanity_check();
        prepared.bench(&mut group);
    }

    group.finish();

    mainnet::bench(c);
}

criterion_group!(benches, evm);
criterion_main!(benches);
