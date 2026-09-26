use crate::fixture::Suites;
use alloy_primitives::Bytes;
use criterion::{BenchmarkGroup, black_box, measurement::WallTime};
use evm2::bytecode::Bytecode;
use evm2_cli::evm_bench::BenchCase;
use std::borrow::Cow;

/// Legacy bytecode analysis of a benchmark transaction's entry contract.
#[derive(Clone, Debug)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    code: Bytes,
}

impl PreparedBench {
    pub(crate) fn load(bench: &BenchCase, suites: &Suites) -> Option<Self> {
        let spec = bench.transaction_spec().expect("transaction benchmark must have a spec");
        let code = suites.get(bench.fixture_path).case(&bench.name, spec).entry_bytecode()?;
        Some(Self { name: bench.name.clone(), code })
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/analysis", self.name), |b| {
            b.iter(|| Bytecode::new_legacy(black_box(self.code.clone())))
        });
    }
}
