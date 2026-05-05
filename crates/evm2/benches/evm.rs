#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};

#[path = "evm/cases.rs"]
mod cases;
#[path = "evm/fixture.rs"]
mod fixture;
#[path = "evm/support.rs"]
mod support;

fn evm(c: &mut Criterion) {
    let mut group = c.benchmark_group("evm");
    let suites = fixture::Suites::load(cases::all().iter().map(|bench| bench.fixture_path));
    for bench in cases::all() {
        let prepared = support::PreparedBench::load(bench, &suites);
        prepared.sanity_check();
        prepared.bench(&mut group);
    }

    group.finish();
}

criterion_group!(benches, evm);
criterion_main!(benches);
