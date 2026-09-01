use criterion::{BenchmarkGroup, black_box, measurement::WallTime};
use evm2_cli::{
    evm_bench::BenchCase,
    replay_bench::{ReplayFixture, diff},
};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

/// revm counterpart of `mainnet::PreparedBench`, plus the harness-only setup
/// benchmarks that price the work both replay paths repeat every iteration.
#[derive(Clone)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    fixture: Arc<ReplayFixture>,
}

impl PreparedBench {
    pub(crate) fn load(bench: &BenchCase) -> Self {
        let path = workspace_path(bench.fixture_path);
        Self { name: bench.name.clone(), fixture: Arc::new(ReplayFixture::load(&path)) }
    }

    /// Replays the corpus through both engines once and refuses to benchmark
    /// unless every transaction agrees on gas used, success and log count.
    pub(crate) fn sanity_check(&self) {
        let evm2 = self.fixture.replay_evm2();
        let revm = self.fixture.replay_revm();
        for (engine, outcome) in [("evm2", &evm2), ("revm", &revm)] {
            let mismatches = outcome.header_gas_mismatches();
            assert!(
                mismatches.is_empty(),
                "{} {engine} replay disagrees with the fixture header gas: {mismatches:#?}",
                self.name
            );
        }
        let mismatches = diff(&evm2, &revm);
        assert!(
            mismatches.is_empty(),
            "{} evm2/revm replay differ on {} entries: {mismatches:#?}",
            self.name,
            mismatches.len()
        );
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/replay/revm", self.name), |b| {
            b.iter(|| black_box(self.fixture.replay_revm()));
        });
    }

    /// Benchmarks the per-iteration harness work only: decoding the fixture's
    /// pre-state into an in-memory database and building every block's
    /// transaction environments, with no EVM execution.
    pub(crate) fn bench_setup(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/replay/setup", self.name), |b| {
            b.iter(|| black_box(self.fixture.setup_evm2()));
        });
        group.bench_function(format!("{}/replay/revm/setup", self.name), |b| {
            b.iter(|| black_box(self.fixture.setup_revm()));
        });
    }
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}
