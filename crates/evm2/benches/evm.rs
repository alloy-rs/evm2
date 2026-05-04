#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};

#[path = "evm/cases.rs"]
mod cases;
#[path = "evm/support.rs"]
mod support;

fn evm(c: &mut Criterion) {
    let mut group = c.benchmark_group("evm");
    for bench in cases::all() {
        let prepared = support::PreparedBench::load(bench);
        prepared.sanity_check();
        prepared.bench(&mut group);
    }

    group.finish();
}

criterion_group!(benches, evm);
criterion_main!(benches);
