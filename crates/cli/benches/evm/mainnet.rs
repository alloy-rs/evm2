use crate::cases::BenchCase;
use criterion::{BenchmarkGroup, black_box, measurement::WallTime};
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, EntryPoint,
    blockchaintest::BlockchainTest, execute_blockchain_tests_suite,
};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    path: PathBuf,
    suite: Arc<BlockchainTest>,
    entrypoint: EntryPoint,
}

impl PreparedBench {
    pub(crate) fn load(bench: &BenchCase) -> Self {
        let path = workspace_path(bench.fixture_path);
        let suite = evm2_eest::read_blockchain_fixture(&path)
            .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()));
        Self {
            name: bench.name.clone(),
            path,
            suite: Arc::new(suite),
            entrypoint: EntryPoint::default(),
        }
    }

    pub(crate) fn sanity_check(&self) {
        let mut hook = BlockchainTestNoopHook;
        let summary = execute_blockchain_tests_suite(
            &self.path,
            &self.suite,
            BlockchainTestExecuteConfig::default(),
            &self.entrypoint,
            &mut hook,
        )
        .unwrap_or_else(|err| panic!("{} fixture sanity check failed: {err}", self.name));
        assert_eq!(summary.executed, 1);
        assert_eq!(summary.skipped, 0);
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/replay", self.name), |b| {
            b.iter(|| {
                let mut hook = BlockchainTestNoopHook;
                black_box(
                    execute_blockchain_tests_suite(
                        &self.path,
                        &self.suite,
                        BlockchainTestExecuteConfig {
                            validate_post_state: false,
                            ..Default::default()
                        },
                        &self.entrypoint,
                        &mut hook,
                    )
                    .unwrap_or_else(|err| panic!("{} fixture replay failed: {err}", self.name)),
                )
            });
        });
    }
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}
