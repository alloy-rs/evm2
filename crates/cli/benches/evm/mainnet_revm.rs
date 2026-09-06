use criterion::{BenchmarkGroup, measurement::WallTime};
use evm2_cli::{
    evm_bench::BenchCase,
    replay_bench::{ReplayFixture, diff},
};
use evm2_eest::BlockchainTestNoopHook;
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

/// revm counterpart of `mainnet::PreparedBench`.
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
    /// unless every transaction agrees on gas used, success and logs, every
    /// block agrees on its EIP-8037 gas split, and both reproduce the header gas.
    pub(crate) fn sanity_check(&self) {
        let evm2 = self.fixture.replay_evm2();
        let revm = self.fixture.replay_revm();
        for (engine, outcome) in [("evm2", &evm2), ("revm", &revm)] {
            assert_eq!(outcome.blocks.len(), self.fixture.blocks());
            assert_eq!(outcome.transactions(), self.fixture.transactions());
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
            b.iter(|| self.fixture.execute_revm(&mut BlockchainTestNoopHook));
        });
    }
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}
